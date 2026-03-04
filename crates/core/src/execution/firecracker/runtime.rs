use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::super::{
    shell_quote, AgentExecutionSpec, AgentProvider, AttemptSpec, ExecutionError,
    FirecrackerBackendConfig, UserPackageDir,
};
use super::{
    network::{host_proxy_egress_policy, HostProxyEgressPolicy, NetworkLease},
    EXT4_BLOCK_SIZE_BYTES,
};

pub(super) struct FirecrackerRuntimeStager<'a> {
    firecracker: &'a FirecrackerBackendConfig,
}

impl<'a> FirecrackerRuntimeStager<'a> {
    pub(super) fn new(firecracker: &'a FirecrackerBackendConfig) -> Self {
        Self { firecracker }
    }

    pub(super) fn stage_runtime_tree(
        &self,
        spec: &AttemptSpec,
        run_dir: &Path,
        network_lease: Option<&NetworkLease>,
    ) -> Result<PreparedRuntimeTree, ExecutionError> {
        let stage_root = run_dir.join("runtime-tree");
        if stage_root.exists() {
            fs::remove_dir_all(&stage_root)?;
        }

        let workspace_dir = stage_root.join("workspace");
        let packages_dir = stage_root.join("openoman-packages");
        let config_dir = stage_root.join("openoman-config");
        let output_dir = stage_root.join("openoman-output");

        fs::create_dir_all(&workspace_dir)?;
        fs::create_dir_all(&packages_dir)?;
        fs::create_dir_all(&config_dir)?;
        fs::create_dir_all(&output_dir)?;

        copy_tree_with_tar(&spec.workspace_dir, &workspace_dir)?;

        let mut manifest = String::new();
        let mut guest_path_entries = Vec::new();
        for (idx, package_dir) in self.firecracker.user_package_dirs.iter().enumerate() {
            let staged_name = format!("package-{idx:03}");
            let staged_dir = packages_dir.join(&staged_name);
            copy_tree_with_tar(&package_dir.host_path, &staged_dir)?;
            manifest.push_str(&format!(
                "/mnt/runtime/openoman-packages/{staged_name}\t{}\t{}\n",
                package_dir.guest_path.display(),
                if package_dir.add_to_path { "1" } else { "0" }
            ));
            if package_dir.add_to_path {
                guest_path_entries.push(package_dir.guest_path.display().to_string());
            }
        }

        fs::write(config_dir.join("instruction.txt"), &spec.instruction)?;
        fs::write(config_dir.join("package-mounts.tsv"), manifest)?;
        if let Some(auth_file) = &spec.agent.auth_file {
            fs::copy(auth_file, config_dir.join("agent-auth.json"))?;
        }
        fs::write(
            config_dir.join("agent.env"),
            render_agent_env(
                &spec.agent,
                &guest_path_entries,
                &self.firecracker.user_package_dirs,
                network_lease,
            ),
        )?;
        fs::write(
            config_dir.join("guest-init-contract.txt"),
            "mount /dev/vdb at /mnt/runtime, source openoman-config/agent.env, bind package mounts, run the agent in /mnt/runtime/workspace, write report/logs to /mnt/runtime/openoman-output\n",
        )?;

        let image_size_bytes = compute_image_size_bytes(&stage_root, spec.limits.disk_quota_bytes)?;
        let image_path = run_dir.join("runtime.ext4");
        let blocks = image_size_bytes.div_ceil(EXT4_BLOCK_SIZE_BYTES);
        let mkfs_args = vec![
            "-F".to_string(),
            "-b".to_string(),
            EXT4_BLOCK_SIZE_BYTES.to_string(),
            "-d".to_string(),
            stage_root.display().to_string(),
            image_path.display().to_string(),
            blocks.to_string(),
        ];
        run_command("mkfs.ext4", &mkfs_args)?;

        Ok(PreparedRuntimeTree { image_path })
    }
}

#[derive(Debug)]
pub(super) struct PreparedRuntimeTree {
    pub(super) image_path: PathBuf,
}

pub(super) fn render_agent_env(
    agent: &AgentExecutionSpec,
    guest_path_entries: &[String],
    user_package_dirs: &[UserPackageDir],
    network_lease: Option<&NetworkLease>,
) -> String {
    let egress_policy = host_proxy_egress_policy(&agent.egress_allowed_domains);
    let mut env_file = String::new();
    env_file.push_str(&format!(
        "AGENT_PROVIDER={}\n",
        match agent.provider {
            AgentProvider::Codex => "codex",
            AgentProvider::Cursor => "cursor",
        }
    ));
    env_file.push_str(&format!(
        "AGENT_BIN={}\n",
        shell_quote(&resolve_guest_agent_bin(&agent.bin, user_package_dirs))
    ));
    if let Some(model) = &agent.model {
        env_file.push_str(&format!("AGENT_MODEL={}\n", shell_quote(model)));
    }
    if agent.auth_file.is_some() {
        env_file
            .push_str("OPENOMAN_AGENT_AUTH_FILE='/mnt/runtime/openoman-config/agent-auth.json'\n");
    }
    if let Some(api_key) = &agent.api_key {
        env_file.push_str(&format!("export CURSOR_API_KEY={}\n", shell_quote(api_key)));
    }
    if let Some(proxy) = &agent.egress_proxy {
        env_file.push_str(&format!("export HTTPS_PROXY={}\n", shell_quote(proxy)));
        env_file.push_str(&format!("export HTTP_PROXY={}\n", shell_quote(proxy)));
        env_file.push_str(&format!("export https_proxy={}\n", shell_quote(proxy)));
        env_file.push_str(&format!("export http_proxy={}\n", shell_quote(proxy)));
    }
    if !agent.egress_allowed_domains.is_empty() {
        env_file.push_str(&format!(
            "OPENOMAN_EGRESS_ALLOWED_DOMAINS={}\n",
            shell_quote(&agent.egress_allowed_domains.join(","))
        ));
    }
    if let Some(lease) = network_lease {
        env_file.push_str("OPENOMAN_NET_MODE='host-proxy'\n");
        env_file.push_str(&format!(
            "OPENOMAN_NET_IFACE={}\n",
            shell_quote(&lease.guest_iface)
        ));
        env_file.push_str(&format!(
            "OPENOMAN_NET_GUEST_IPV4={}\n",
            shell_quote(&lease.guest_ip_cidr())
        ));
        env_file.push_str(&format!(
            "OPENOMAN_NET_HOST_PROXY_URL={}\n",
            shell_quote(&lease.host_proxy_url())
        ));
        env_file.push_str(&format!(
            "OPENOMAN_NET_HOST_IPV4={}\n",
            shell_quote(&lease.host_ip.to_string())
        ));
        if matches!(egress_policy, HostProxyEgressPolicy::AllowAllDebug) {
            env_file.push_str("OPENOMAN_NET_ALLOW_ALL='1'\n");
            let dns_servers = discover_host_dns_servers();
            if !dns_servers.is_empty() {
                env_file.push_str(&format!(
                    "OPENOMAN_NET_DNS_SERVERS={}\n",
                    shell_quote(&dns_servers.join(","))
                ));
            }
        }
        env_file.push_str(&format!(
            "export HTTPS_PROXY={}\n",
            shell_quote(&lease.host_proxy_url())
        ));
        env_file.push_str(&format!(
            "export HTTP_PROXY={}\n",
            shell_quote(&lease.host_proxy_url())
        ));
        env_file.push_str(&format!(
            "export https_proxy={}\n",
            shell_quote(&lease.host_proxy_url())
        ));
        env_file.push_str(&format!(
            "export http_proxy={}\n",
            shell_quote(&lease.host_proxy_url())
        ));
    }
    env_file.push_str("WORKSPACE_DIR='/mnt/runtime/workspace'\n");
    env_file.push_str("OUTPUT_DIR='/mnt/runtime/openoman-output'\n");
    env_file.push_str("INSTRUCTION_FILE='/mnt/runtime/openoman-config/instruction.txt'\n");
    env_file.push_str("PACKAGE_MOUNTS_FILE='/mnt/runtime/openoman-config/package-mounts.tsv'\n");
    env_file.push_str(&format!(
        "OPENOMAN_EXTRA_PATH={}\n",
        shell_quote(&guest_path_entries.join(":"))
    ));
    env_file
}

pub(super) fn compute_image_size_bytes(
    root: &Path,
    disk_quota_bytes: u64,
) -> Result<u64, ExecutionError> {
    let source_size = directory_size_bytes(root)?;
    let minimum = 64 * 1024 * 1024_u64;
    let overhead = 128 * 1024 * 1024_u64;
    let requested = (source_size.saturating_add(overhead)).max(minimum);
    if requested > disk_quota_bytes {
        return Err(ExecutionError::InvalidConfig(format!(
            "prepared runtime input needs {requested} bytes but disk quota is only {disk_quota_bytes} bytes"
        )));
    }
    Ok(requested)
}

fn discover_host_dns_servers() -> Vec<String> {
    let mut servers = Vec::new();

    for path in ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"] {
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        for line in contents.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("nameserver ") {
                continue;
            }
            let value = trimmed["nameserver ".len()..].trim();
            if value.is_empty() || value == "127.0.0.1" || value == "127.0.0.53" || value == "::1" {
                continue;
            }
            if !servers.iter().any(|existing| existing == value) {
                servers.push(value.to_string());
            }
        }
        if !servers.is_empty() {
            break;
        }
    }

    servers
}

fn resolve_guest_agent_bin(bin: &str, user_package_dirs: &[UserPackageDir]) -> String {
    let agent_path = Path::new(bin);
    if agent_path.is_absolute() {
        for package_dir in user_package_dirs {
            if let Ok(relative) = agent_path.strip_prefix(&package_dir.host_path) {
                return package_dir.guest_path.join(relative).display().to_string();
            }
        }
    }

    bin.to_string()
}

fn directory_size_bytes(path: &Path) -> Result<u64, ExecutionError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }

    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        total = total.saturating_add(directory_size_bytes(&entry.path())?);
    }
    Ok(total)
}

fn copy_tree_with_tar(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
    fs::create_dir_all(destination)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let archive = std::env::temp_dir().join(format!(
        "openoman-copy-{}-{}.tar",
        std::process::id(),
        nonce
    ));
    let archive_name = archive.to_str().unwrap_or("tmp-copy.tar");
    let result = (|| {
        run_tar(source, &["-cf", archive_name, "."])?;
        run_tar(destination, &["-xf", archive_name])?;
        Ok(())
    })();
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    result
}

fn run_tar(cwd: &Path, args: &[&str]) -> Result<(), ExecutionError> {
    let output = Command::new("tar").current_dir(cwd).args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(ExecutionError::CommandFailed {
        program: "tar".to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_command(program: &str, args: &[String]) -> Result<(), ExecutionError> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(ExecutionError::CommandFailed {
        program: program.to_string(),
        args: args.to_vec(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}
