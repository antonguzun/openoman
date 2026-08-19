use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use crate::agents::{build_launch_plan, AgentLaunchContext, AgentLaunchPlan, AgentReportMode};

use super::super::{
    shell_quote, AgentExecutionSpec, AttemptSpec, DockerAuthConfig, ExecutionError,
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
            let staged_auth_file = config_dir.join("agent-auth.json");
            fs::copy(auth_file, &staged_auth_file)?;
            fs::set_permissions(&staged_auth_file, fs::Permissions::from_mode(0o600))?;
        }
        if let Some(docker_auth_config) = &self.firecracker.docker_auth_config {
            let docker_auth_contents = load_docker_auth_contents(docker_auth_config)?;
            write_secret_file(
                &config_dir.join("docker-config.json"),
                docker_auth_contents.as_bytes(),
            )?;
        }
        write_secret_file(
            &config_dir.join("agent.env"),
            render_agent_env(
                &spec.agent,
                &spec.instruction,
                &guest_path_entries,
                &self.firecracker.user_package_dirs,
                self.firecracker.docker_daemon,
                self.firecracker.docker_auth_config.is_some(),
                network_lease,
            )?
            .as_bytes(),
        )?;
        fs::write(
            config_dir.join("guest-init-contract.txt"),
            "mount /dev/vdb at /mnt/runtime, source openoman-config/agent.env, optionally start dockerd, bind package mounts, run the agent in /mnt/runtime/workspace, write report/logs to /mnt/runtime/openoman-output\n",
        )?;

        let minimum_image_size_bytes = compute_image_size_bytes(&stage_root)?;
        let image_size_bytes = match self.firecracker.runtime_disk_bytes {
            Some(explicit_size_bytes) => {
                if explicit_size_bytes > spec.limits.disk_quota_bytes {
                    return Err(ExecutionError::InvalidConfig(format!(
                        "configured Firecracker runtime disk size {} bytes exceeds disk quota {} bytes",
                        explicit_size_bytes, spec.limits.disk_quota_bytes
                    )));
                }
                if explicit_size_bytes < minimum_image_size_bytes {
                    return Err(ExecutionError::InvalidConfig(format!(
                        "configured Firecracker runtime disk size {} bytes is smaller than prepared runtime input needs {} bytes",
                        explicit_size_bytes, minimum_image_size_bytes
                    )));
                }
                explicit_size_bytes
            }
            None => {
                if minimum_image_size_bytes > spec.limits.disk_quota_bytes {
                    return Err(ExecutionError::InvalidConfig(format!(
                        "prepared runtime input needs {} bytes but disk quota is only {} bytes",
                        minimum_image_size_bytes, spec.limits.disk_quota_bytes
                    )));
                }
                minimum_image_size_bytes
            }
        };
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
        // The image embeds agent.env with the agent credential in clear text.
        fs::set_permissions(&image_path, fs::Permissions::from_mode(0o600))?;

        Ok(PreparedRuntimeTree { image_path })
    }
}

// Credential-carrying files must never be readable by other local users; 0600 at
// creation avoids the chmod-after-write window a plain fs::write would leave.
fn write_secret_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)
}

#[derive(Debug)]
pub(super) struct PreparedRuntimeTree {
    pub(super) image_path: PathBuf,
}

pub(super) fn render_agent_env(
    agent: &AgentExecutionSpec,
    instruction: &str,
    guest_path_entries: &[String],
    user_package_dirs: &[UserPackageDir],
    docker_daemon_enabled: bool,
    docker_auth_config_enabled: bool,
    network_lease: Option<&NetworkLease>,
) -> Result<String, ExecutionError> {
    let egress_policy = host_proxy_egress_policy(&agent.egress_allowed_domains);
    let resolved_bin = resolve_guest_agent_bin(&agent.bin, user_package_dirs);
    let launch_plan = build_launch_plan(
        agent,
        AgentLaunchContext {
            binary: &resolved_bin,
            workspace_dir: "/mnt/runtime/workspace",
            report_path: "/mnt/runtime/openoman-output/report.txt",
            instruction,
        },
    )?;
    let mut env_file = String::new();
    env_file.push_str(&format!(
        "AGENT_PROVIDER={}\n",
        shell_quote(launch_plan.provider_id)
    ));
    env_file.push_str(&format!("AGENT_BIN={}\n", shell_quote(&launch_plan.binary)));
    if let Some(model) = &agent.model {
        env_file.push_str(&format!("AGENT_MODEL={}\n", shell_quote(model)));
    }
    if agent.auth_file.is_some() {
        env_file
            .push_str("OPENOMAN_AGENT_AUTH_FILE='/mnt/runtime/openoman-config/agent-auth.json'\n");
    }
    if let Some(relative_path) = &launch_plan.auth_file_home_relative_path {
        env_file.push_str(&format!(
            "OPENOMAN_AGENT_AUTH_INSTALL_PATH={}\n",
            shell_quote(&format!("/root/{}", relative_path.display()))
        ));
    }
    if docker_auth_config_enabled {
        env_file.push_str(
            "OPENOMAN_DOCKER_AUTH_FILE='/mnt/runtime/openoman-config/docker-config.json'\n",
        );
    }
    if docker_daemon_enabled {
        env_file.push_str("OPENOMAN_DOCKER_DAEMON_ENABLED='1'\n");
    }
    // Truthful only on this backend: the agent runs as uid 0 inside a microVM.
    // Claude Code requires IS_SANDBOX=1 before it accepts
    // --dangerously-skip-permissions under root; the process backend must never
    // set it because there the agent runs directly on the host.
    env_file.push_str("export IS_SANDBOX='1'\n");
    append_agent_launch_plan_env(&mut env_file, &launch_plan);
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
    Ok(env_file)
}

fn load_docker_auth_contents(config: &DockerAuthConfig) -> Result<String, ExecutionError> {
    match config {
        DockerAuthConfig::HostFile(path) => fs::read_to_string(path).map_err(ExecutionError::Io),
        DockerAuthConfig::InlineJson(contents) => Ok(contents.clone()),
    }
}

fn append_agent_launch_plan_env(env_file: &mut String, launch_plan: &AgentLaunchPlan) {
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_ID={}\n",
        shell_quote(launch_plan.provider_id)
    ));
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_BIN={}\n",
        shell_quote(&launch_plan.binary)
    ));
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_WORKDIR={}\n",
        shell_quote(&launch_plan.working_directory)
    ));
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_REPORT_MODE={}\n",
        shell_quote(match launch_plan.report_mode {
            AgentReportMode::File => "file",
            AgentReportMode::Stdout => "stdout",
        })
    ));
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_REDACT_API_KEY_ARGS={}\n",
        if launch_plan.redact_api_key_args {
            "'1'"
        } else {
            "'0'"
        }
    ));
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_VERSION_ARG_COUNT='{}'\n",
        launch_plan.version_probe_args.len()
    ));
    for (idx, arg) in launch_plan.version_probe_args.iter().enumerate() {
        env_file.push_str(&format!(
            "OPENOMAN_AGENT_VERSION_ARG_{idx:03}={}\n",
            shell_quote(arg)
        ));
    }
    env_file.push_str(&format!(
        "OPENOMAN_AGENT_ARG_COUNT='{}'\n",
        launch_plan.args.len()
    ));
    for (idx, arg) in launch_plan.args.iter().enumerate() {
        env_file.push_str(&format!(
            "OPENOMAN_AGENT_ARG_{idx:03}={}\n",
            shell_quote(arg)
        ));
    }
    for (key, value) in &launch_plan.env {
        env_file.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
}

pub(super) fn compute_image_size_bytes(root: &Path) -> Result<u64, ExecutionError> {
    let source_size = directory_size_bytes(root)?;
    let minimum = 64 * 1024 * 1024_u64;
    let overhead = 128 * 1024 * 1024_u64;
    Ok((source_size.saturating_add(overhead)).max(minimum))
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
