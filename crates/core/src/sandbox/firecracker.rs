use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde::Serialize;

use super::{
    shell_quote, AgentExecutionSpec, AgentProvider, AttemptSpec, CollectedSandboxOutput,
    FirecrackerBackendConfig, FirecrackerMode, FirecrackerNetworkPrivilegeMode,
    FirecrackerNetworkingMode, SandboxBackend, SandboxBackendKind, SandboxError, SandboxHandle,
    SandboxRunner, SandboxRuntimeConfig, UserPackageDir,
};

const EXT4_BLOCK_SIZE_BYTES: u64 = 4096;
const FIRECRACKER_NET_HELPER_SUBCOMMAND: &[&str] = &["internal", "firecracker-net"];
const PROXY_REQUEST_LIMIT_BYTES: usize = 16 * 1024;

#[derive(Debug)]
pub struct FirecrackerBackend {
    runtime: SandboxRuntimeConfig,
    firecracker: FirecrackerBackendConfig,
}

impl FirecrackerBackend {
    pub fn new(runtime: SandboxRuntimeConfig) -> Result<Self, SandboxError> {
        let Some(firecracker) = runtime.firecracker.clone() else {
            return Err(SandboxError::InvalidConfig(
                "sandbox.firecracker settings are required for backend = \"firecracker\""
                    .to_string(),
            ));
        };

        Ok(Self {
            runtime,
            firecracker,
        })
    }
}

impl SandboxBackend for FirecrackerBackend {
    fn kind(&self) -> SandboxBackendKind {
        SandboxBackendKind::Firecracker
    }

    fn check_runtime_dependencies(&self) -> Result<(), SandboxError> {
        validate_runtime_dir(&self.runtime.runtime_dir)?;
        validate_firecracker_common(&self.firecracker)?;

        match self.firecracker.mode {
            FirecrackerMode::Direct => {
                ensure_command_exists("tar")?;
                ensure_command_exists("mkfs.ext4")?;
                ensure_command_exists("debugfs")?;
                ensure_command_exists("e2fsck")?;
                ensure_command_exists(&self.firecracker.firecracker_bin)?;

                let kvm = Path::new("/dev/kvm");
                if !kvm.exists() {
                    return Err(SandboxError::MissingDependency(
                        "/dev/kvm is required for firecracker direct mode".to_string(),
                    ));
                }

                validate_regular_file(
                    &self.firecracker.kernel_image_path,
                    "sandbox.firecracker.kernel_image_path",
                )?;
                validate_regular_file(
                    &self.firecracker.rootfs_image_path,
                    "sandbox.firecracker.rootfs_image_path",
                )?;
                validate_user_package_dirs(&self.firecracker.user_package_dirs)?;
                if self.firecracker.networking.mode == FirecrackerNetworkingMode::HostProxy {
                    ensure_command_exists("ip")?;
                    if self.firecracker.networking.privilege_mode
                        == FirecrackerNetworkPrivilegeMode::Sudo
                    {
                        ensure_command_exists("sudo")?;
                    }
                }
                Ok(())
            }
            FirecrackerMode::Jailer => {
                ensure_command_exists(&self.firecracker.jailer_bin)?;
                Err(SandboxError::NotImplemented(
                    "sandbox backend firecracker(jailer) needs additional privileged environment preparation; use sandbox.firecracker.mode = \"direct\" in this release".to_string(),
                ))
            }
        }
    }

    fn create_runner(&self) -> Result<Box<dyn SandboxRunner>, SandboxError> {
        match self.firecracker.mode {
            FirecrackerMode::Direct => Ok(Box::new(FirecrackerDirectRunner::new(
                self.runtime.runtime_dir.clone(),
                self.firecracker.clone(),
            ))),
            FirecrackerMode::Jailer => Err(SandboxError::NotImplemented(
                "sandbox backend firecracker(jailer) needs additional privileged environment preparation; use sandbox.firecracker.mode = \"direct\" in this release".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
struct NetworkLease {
    tap_name: String,
    guest_iface: String,
    host_ip: Ipv4Addr,
    guest_ip: Ipv4Addr,
    prefix_len: u8,
    guest_mac: String,
    proxy_port: u16,
}

impl NetworkLease {
    fn guest_ip_cidr(&self) -> String {
        format!("{}/{}", self.guest_ip, self.prefix_len)
    }

    fn host_proxy_url(&self) -> String {
        format!("http://{}:{}", self.host_ip, self.proxy_port)
    }
}

#[derive(Debug)]
struct NetworkProxyHandle {
    bind_addr: SocketAddrV4,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl NetworkProxyHandle {
    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.bind_addr);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
struct RunningFirecrackerVm {
    child: Child,
    timeout: Duration,
    network_lease: Option<NetworkLease>,
    network_proxy: Option<NetworkProxyHandle>,
}

#[derive(Debug)]
pub struct FirecrackerDirectRunner {
    root_dir: PathBuf,
    firecracker: FirecrackerBackendConfig,
    next_handle_id: u64,
    running: HashMap<u64, RunningFirecrackerVm>,
}

impl FirecrackerDirectRunner {
    fn new(root_dir: PathBuf, firecracker: FirecrackerBackendConfig) -> Self {
        Self {
            root_dir,
            firecracker,
            next_handle_id: 1,
            running: HashMap::new(),
        }
    }

    fn stable_output_dir(&self, job_id: &str, attempt_id: u32) -> PathBuf {
        self.root_dir
            .join("jobs")
            .join(job_id)
            .join(format!("attempt-{attempt_id}"))
    }

    fn run_command(&self, program: &str, args: &[String]) -> Result<(), SandboxError> {
        let output = Command::new(program).args(args).output()?;
        if output.status.success() {
            return Ok(());
        }

        Err(SandboxError::CommandFailed {
            program: program.to_string(),
            args: args.to_vec(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn run_command_allowing_codes(
        &self,
        program: &str,
        args: &[String],
        allowed: &[i32],
    ) -> Result<(), SandboxError> {
        let output = Command::new(program).args(args).output()?;
        let status_code = output.status.code().unwrap_or(-1);
        if output.status.success() || allowed.contains(&status_code) {
            return Ok(());
        }

        Err(SandboxError::CommandFailed {
            program: program.to_string(),
            args: args.to_vec(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn run_tar(&self, cwd: &Path, args: &[&str]) -> Result<(), SandboxError> {
        let output = Command::new("tar").current_dir(cwd).args(args).output()?;
        if output.status.success() {
            return Ok(());
        }

        Err(SandboxError::CommandFailed {
            program: "tar".to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn copy_tree_with_tar(&self, source: &Path, destination: &Path) -> Result<(), SandboxError> {
        fs::create_dir_all(destination)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let archive = std::env::temp_dir().join(format!(
            "openoman-copy-{}-{}-{}.tar",
            std::process::id(),
            self.next_handle_id,
            nonce
        ));
        let archive_name = archive.to_str().unwrap_or("tmp-copy.tar");
        let result = (|| {
            self.run_tar(source, &["-cf", archive_name, "."])?;
            self.run_tar(destination, &["-xf", archive_name])?;
            Ok(())
        })();
        if archive.exists() {
            fs::remove_file(&archive)?;
        }
        result
    }

    fn stage_runtime_tree(
        &self,
        spec: &AttemptSpec,
        run_dir: &Path,
        network_lease: Option<&NetworkLease>,
    ) -> Result<PreparedRuntimeTree, SandboxError> {
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

        self.copy_tree_with_tar(&spec.workspace_dir, &workspace_dir)?;

        let mut manifest = String::new();
        let mut guest_path_entries = Vec::new();
        for (idx, package_dir) in self.firecracker.user_package_dirs.iter().enumerate() {
            let staged_name = format!("package-{idx:03}");
            let staged_dir = packages_dir.join(&staged_name);
            self.copy_tree_with_tar(&package_dir.host_path, &staged_dir)?;
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
        if let Some(auth_file) = &spec.agent.codex_auth_file {
            fs::copy(auth_file, config_dir.join("codex-auth.json"))?;
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
        self.run_command("mkfs.ext4", &mkfs_args)?;

        Ok(PreparedRuntimeTree { image_path })
    }

    fn write_firecracker_config(
        &self,
        run_dir: &Path,
        rootfs_image_path: &Path,
        runtime_image_path: &Path,
        spec: &AttemptSpec,
        network_lease: Option<&NetworkLease>,
    ) -> Result<PathBuf, SandboxError> {
        let config_path = run_dir.join("firecracker-config.json");
        let config = FirecrackerConfigFile {
            boot_source: BootSourceConfig {
                kernel_image_path: self.firecracker.kernel_image_path.display().to_string(),
                boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/openoman-init".to_string(),
            },
            drives: vec![
                DriveConfig {
                    drive_id: "rootfs".to_string(),
                    path_on_host: rootfs_image_path.display().to_string(),
                    is_root_device: true,
                    is_read_only: false,
                },
                DriveConfig {
                    drive_id: "runtime".to_string(),
                    path_on_host: runtime_image_path.display().to_string(),
                    is_root_device: false,
                    is_read_only: false,
                },
            ],
            machine_config: MachineConfig {
                vcpu_count: spec.limits.vcpu_count.max(1),
                mem_size_mib: spec.limits.memory_mib.max(128),
                smt: false,
            },
            network_interfaces: network_lease
                .map(|lease| {
                    vec![NetworkInterfaceConfig {
                        iface_id: lease.guest_iface.clone(),
                        host_dev_name: lease.tap_name.clone(),
                        guest_mac: lease.guest_mac.clone(),
                    }]
                })
                .unwrap_or_default(),
            entropy: EntropyDeviceConfig {},
        };

        fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config).map_err(|err| {
                SandboxError::InvalidConfig(format!(
                    "failed to serialize firecracker config: {err}"
                ))
            })?,
        )?;
        Ok(config_path)
    }

    fn runtime_image_path(handle: &SandboxHandle) -> PathBuf {
        handle.run_dir.join("runtime.ext4")
    }

    fn rootfs_copy_path(handle: &SandboxHandle) -> PathBuf {
        handle.run_dir.join("rootfs.ext4")
    }

    fn serial_log_path(handle: &SandboxHandle) -> PathBuf {
        handle.run_dir.join("serial.log")
    }

    fn vmm_log_path(handle: &SandboxHandle) -> PathBuf {
        handle.run_dir.join("firecracker.log")
    }

    fn network_log_path(handle: &SandboxHandle) -> PathBuf {
        handle.run_dir.join("network.log")
    }

    fn dump_file_from_image(
        &self,
        image_path: &Path,
        image_file_path: &str,
        host_path: &Path,
    ) -> Result<(), SandboxError> {
        let args = vec![
            "-R".to_string(),
            format!("dump -p {image_file_path} {}", host_path.display()),
            image_path.display().to_string(),
        ];
        self.run_command("debugfs", &args)
    }

    fn dump_workspace_from_image(
        &self,
        image_path: &Path,
        output_dir: &Path,
    ) -> Result<PathBuf, SandboxError> {
        let parent = output_dir.join("workspace-dump");
        if parent.exists() {
            fs::remove_dir_all(&parent)?;
        }
        fs::create_dir_all(&parent)?;

        let args = vec![
            "-R".to_string(),
            format!("rdump /workspace {}", parent.display()),
            image_path.display().to_string(),
        ];
        self.run_command("debugfs", &args)?;

        let dumped_workspace = parent.join("workspace");
        if !dumped_workspace.exists() {
            return Err(SandboxError::RunnerState(format!(
                "debugfs did not produce {}",
                dumped_workspace.display()
            )));
        }

        let final_path = output_dir.join("workspace-result");
        if final_path.exists() {
            fs::remove_dir_all(&final_path)?;
        }
        fs::rename(&dumped_workspace, &final_path)?;
        fs::remove_dir_all(&parent)?;
        Ok(final_path)
    }

    fn read_exit_code_marker(&self, image_path: &Path) -> Result<Option<i32>, SandboxError> {
        let args = vec![
            "-R".to_string(),
            "cat /openoman-output/exit-code.txt".to_string(),
            image_path.display().to_string(),
        ];
        let output = Command::new("debugfs").args(&args).output()?;
        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(value) = stdout.lines().map(str::trim).find(|line| !line.is_empty()) else {
            return Ok(None);
        };
        let code = value.parse::<i32>().map_err(|err| {
            SandboxError::RunnerState(format!("invalid exit-code marker '{value}': {err}"))
        })?;
        Ok(Some(code))
    }

    fn configure_networking(
        &self,
        handle: &SandboxHandle,
        spec: &AttemptSpec,
    ) -> Result<(Option<NetworkLease>, Option<NetworkProxyHandle>), SandboxError> {
        if self.firecracker.networking.mode != FirecrackerNetworkingMode::HostProxy {
            return Ok((None, None));
        }
        if spec.agent.egress_allowed_domains.is_empty() {
            return Err(SandboxError::InvalidConfig(
                "agent.egress_allowed_domains must include at least one domain when sandbox.firecracker.networking.mode = host-proxy"
                    .to_string(),
            ));
        }

        let lease = allocate_network_lease(
            &self.firecracker.networking.subnet_cidr,
            handle.id,
            &self.firecracker.networking.tap_name_prefix,
            self.firecracker.networking.proxy_port,
        )?;
        let log_path = Self::network_log_path(handle);
        self.append_network_log_line(
            &log_path,
            &format!(
                "network setup requested: tap={} host_ip={}/{} guest_ip={}/{} proxy_port={} allowlist={}",
                lease.tap_name,
                lease.host_ip,
                lease.prefix_len,
                lease.guest_ip,
                lease.prefix_len,
                lease.proxy_port,
                spec.agent.egress_allowed_domains.join(",")
            ),
        )?;
        self.run_network_helper_command(
            "setup",
            &[
                ("--tap-name", lease.tap_name.clone()),
                ("--host-ip", lease.host_ip.to_string()),
                ("--prefix-len", lease.prefix_len.to_string()),
            ],
            &log_path,
        )?;
        match self.start_network_proxy(&lease, &spec.agent.egress_allowed_domains, &log_path) {
            Ok(proxy) => Ok((Some(lease), Some(proxy))),
            Err(err) => {
                let _ = self.teardown_network_lease(&lease, &log_path);
                Err(err)
            }
        }
    }

    fn append_network_log_line(&self, log_path: &Path, message: &str) -> Result<(), SandboxError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        writeln!(file, "{message}")?;
        Ok(())
    }

    fn run_network_helper_command(
        &self,
        action: &str,
        args: &[(&str, String)],
        log_path: &Path,
    ) -> Result<(), SandboxError> {
        let current_exe = std::env::current_exe().map_err(SandboxError::Io)?;
        let mut command = match self.firecracker.networking.privilege_mode {
            FirecrackerNetworkPrivilegeMode::Sudo => {
                let mut command = Command::new("sudo");
                command.arg("-n").arg(&current_exe);
                command
            }
            FirecrackerNetworkPrivilegeMode::Direct => Command::new(&current_exe),
        };
        command.args(FIRECRACKER_NET_HELPER_SUBCOMMAND).arg(action);
        let mut display_args = Vec::new();
        if self.firecracker.networking.privilege_mode == FirecrackerNetworkPrivilegeMode::Sudo {
            display_args.push("-n".to_string());
            display_args.push(current_exe.display().to_string());
        }
        display_args.extend(
            FIRECRACKER_NET_HELPER_SUBCOMMAND
                .iter()
                .map(|value| (*value).to_string()),
        );
        display_args.push(action.to_string());
        for (flag, value) in args {
            command.arg(flag).arg(value);
            display_args.push((*flag).to_string());
            display_args.push(value.clone());
        }

        let helper_program = match self.firecracker.networking.privilege_mode {
            FirecrackerNetworkPrivilegeMode::Sudo => "sudo".to_string(),
            FirecrackerNetworkPrivilegeMode::Direct => current_exe.display().to_string(),
        };
        self.append_network_log_line(
            log_path,
            &format!(
                "running network helper: {} {}",
                helper_program,
                display_args.join(" ")
            ),
        )?;

        let output = command.output()?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if !stdout.is_empty() {
                self.append_network_log_line(log_path, &format!("helper stdout: {stdout}"))?;
            }
            if !stderr.is_empty() {
                self.append_network_log_line(log_path, &format!("helper stderr: {stderr}"))?;
            }
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        self.append_network_log_line(
            log_path,
            &format!(
                "network helper failed: exit={:?} stderr={stderr}",
                output.status.code()
            ),
        )?;
        Err(SandboxError::CommandFailed {
            program: match self.firecracker.networking.privilege_mode {
                FirecrackerNetworkPrivilegeMode::Sudo => "sudo".to_string(),
                FirecrackerNetworkPrivilegeMode::Direct => current_exe.display().to_string(),
            },
            args: display_args,
            stderr,
        })
    }

    fn start_network_proxy(
        &self,
        lease: &NetworkLease,
        allowed_domains: &[String],
        log_path: &Path,
    ) -> Result<NetworkProxyHandle, SandboxError> {
        let requested_bind_addr = SocketAddrV4::new(lease.host_ip, lease.proxy_port);
        let listener = TcpListener::bind(requested_bind_addr)?;
        let bind_addr = match listener.local_addr()? {
            std::net::SocketAddr::V4(addr) => addr,
            std::net::SocketAddr::V6(_) => {
                return Err(SandboxError::RunnerState(
                    "host proxy unexpectedly bound to an IPv6 address".to_string(),
                ))
            }
        };
        listener.set_nonblocking(true)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let allowed_domains = Arc::new(allowed_domains.to_vec());
        let log_file = Arc::new(Mutex::new(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)?,
        ));
        write_shared_log(&log_file, &format!("host proxy listening on {bind_addr}"));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let log_for_thread = Arc::clone(&log_file);
        let allowed_for_thread = Arc::clone(&allowed_domains);
        let accept_thread = thread::spawn(move || {
            loop {
                if shutdown_for_thread.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, peer_addr)) => {
                        let log_for_client = Arc::clone(&log_for_thread);
                        let allowed_for_client = Arc::clone(&allowed_for_thread);
                        thread::spawn(move || {
                            handle_connect_proxy_client(
                                stream,
                                peer_addr.to_string(),
                                allowed_for_client,
                                log_for_client,
                            );
                        });
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(err) => {
                        write_shared_log(&log_for_thread, &format!("proxy accept failed: {err}"));
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            write_shared_log(&log_for_thread, "host proxy stopped");
        });
        Ok(NetworkProxyHandle {
            bind_addr,
            shutdown,
            accept_thread: Some(accept_thread),
        })
    }

    fn teardown_network_lease(
        &self,
        lease: &NetworkLease,
        log_path: &Path,
    ) -> Result<(), SandboxError> {
        self.append_network_log_line(
            log_path,
            &format!("tearing down tap device {}", lease.tap_name),
        )?;
        self.run_network_helper_command(
            "teardown",
            &[("--tap-name", lease.tap_name.clone())],
            log_path,
        )
    }
}

impl SandboxRunner for FirecrackerDirectRunner {
    fn start(&mut self, spec: AttemptSpec) -> Result<SandboxHandle, SandboxError> {
        let handle_id = self.next_handle_id;
        self.next_handle_id += 1;

        let run_dir = self.root_dir.join("runs").join(format!(
            "{}-attempt-{}-{handle_id}",
            spec.job_id, spec.attempt_id
        ));
        if run_dir.exists() {
            fs::remove_dir_all(&run_dir)?;
        }
        fs::create_dir_all(&run_dir)?;
        let handle = SandboxHandle {
            id: handle_id,
            run_dir: run_dir.clone(),
        };
        let network_log_path = Self::network_log_path(&handle);
        let (network_lease, mut network_proxy) = match self.configure_networking(&handle, &spec) {
            Ok(result) => result,
            Err(err) => {
                if run_dir.exists() {
                    let _ = fs::remove_dir_all(&run_dir);
                }
                return Err(err);
            }
        };

        let rootfs_copy = Self::rootfs_copy_path(&handle);
        fs::copy(&self.firecracker.rootfs_image_path, &rootfs_copy)?;
        let prepared = match self.stage_runtime_tree(&spec, &run_dir, network_lease.as_ref()) {
            Ok(prepared) => prepared,
            Err(err) => {
                if let Some(proxy) = network_proxy.as_mut() {
                    proxy.stop();
                }
                if let Some(lease) = network_lease.as_ref() {
                    let _ = self.teardown_network_lease(lease, &network_log_path);
                }
                return Err(err);
            }
        };
        let config_path = match self.write_firecracker_config(
            &run_dir,
            &rootfs_copy,
            &prepared.image_path,
            &spec,
            network_lease.as_ref(),
        ) {
            Ok(path) => path,
            Err(err) => {
                if let Some(proxy) = network_proxy.as_mut() {
                    proxy.stop();
                }
                if let Some(lease) = network_lease.as_ref() {
                    let _ = self.teardown_network_lease(lease, &network_log_path);
                }
                return Err(err);
            }
        };
        let serial_log = Self::serial_log_path(&handle);
        let serial_log_file = fs::File::create(&serial_log)?;
        let vmm_log = Self::vmm_log_path(&handle);

        let firecracker_bin = resolve_command_path(&self.firecracker.firecracker_bin)?;
        let child = match Command::new(firecracker_bin)
            .arg("--no-api")
            .arg("--id")
            .arg(format!("openoman-{}-{handle_id}", spec.job_id))
            .arg("--log-path")
            .arg(&vmm_log)
            .arg("--level")
            .arg("Info")
            .arg("--config-file")
            .arg(&config_path)
            .stdout(Stdio::from(serial_log_file.try_clone()?))
            .stderr(Stdio::from(serial_log_file))
            .env("OPENOMAN_FAKE_RUNTIME_IMAGE", &prepared.image_path)
            .env(
                "OPENOMAN_FAKE_RUNTIME_ROOTFS",
                &self.firecracker.rootfs_image_path,
            )
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                if let Some(proxy) = network_proxy.as_mut() {
                    proxy.stop();
                }
                if let Some(lease) = network_lease.as_ref() {
                    let _ = self.teardown_network_lease(lease, &network_log_path);
                }
                return Err(SandboxError::Io(err));
            }
        };

        self.running.insert(
            handle_id,
            RunningFirecrackerVm {
                child,
                timeout: Duration::from_secs(spec.limits.timeout_secs.max(1)),
                network_lease,
                network_proxy,
            },
        );

        Ok(handle)
    }

    fn wait(&mut self, handle: &SandboxHandle) -> Result<super::SandboxExitStatus, SandboxError> {
        if !self.running.contains_key(&handle.id) {
            return Err(SandboxError::RunnerState(format!(
                "unknown handle {}",
                handle.id
            )));
        }
        let start = Instant::now();
        let image_path = Self::runtime_image_path(handle);

        loop {
            let status = {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    SandboxError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.try_wait()?
            };
            if let Some(status) = status {
                return Ok(status.into());
            }

            if let Some(code) = self.read_exit_code_marker(&image_path)? {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    SandboxError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.kill()?;
                let _ = running.child.wait();
                return Ok(super::SandboxExitStatus {
                    success: code == 0,
                    code: Some(code),
                    timed_out: false,
                });
            }

            let timeout = self
                .running
                .get(&handle.id)
                .ok_or_else(|| SandboxError::RunnerState(format!("unknown handle {}", handle.id)))?
                .timeout;
            if start.elapsed() >= timeout {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    SandboxError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.kill()?;
                let status = running.child.wait()?;
                return Ok(super::SandboxExitStatus {
                    success: false,
                    code: status.code(),
                    timed_out: true,
                });
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    fn collect_output(
        &self,
        handle: &SandboxHandle,
        job_id: &str,
        attempt_id: u32,
    ) -> Result<CollectedSandboxOutput, SandboxError> {
        let output_dir = self.stable_output_dir(job_id, attempt_id);
        if output_dir.exists() {
            fs::remove_dir_all(&output_dir)?;
        }
        fs::create_dir_all(&output_dir)?;

        let image_path = Self::runtime_image_path(handle);
        self.run_command_allowing_codes(
            "e2fsck",
            &["-fy".to_string(), image_path.display().to_string()],
            &[1, 2],
        )?;

        let modified_workspace_dir = self.dump_workspace_from_image(&image_path, &output_dir)?;
        let report_path = output_dir.join("report.txt");
        let logs_path = output_dir.join("logs.txt");

        if self
            .dump_file_from_image(&image_path, "/openoman-output/report.txt", &report_path)
            .is_err()
            || !report_path.exists()
        {
            fs::write(
                &report_path,
                "Sandbox execution completed without report output.\n",
            )?;
        }

        if self
            .dump_file_from_image(&image_path, "/openoman-output/logs.txt", &logs_path)
            .is_err()
            || !logs_path.exists()
        {
            let serial_log_path = Self::serial_log_path(handle);
            if serial_log_path.exists() {
                fs::copy(serial_log_path, &logs_path)?;
            } else {
                fs::write(&logs_path, "sandbox execution did not produce logs\n")?;
            }
        }
        let network_log_path = Self::network_log_path(handle);
        if network_log_path.exists() {
            append_host_network_log(&logs_path, &network_log_path)?;
        }

        Ok(CollectedSandboxOutput {
            modified_workspace_dir,
            report_path,
            logs_path,
        })
    }

    fn stop(&mut self, handle: &SandboxHandle) -> Result<(), SandboxError> {
        if let Some(mut running) = self.running.remove(&handle.id) {
            if running.child.try_wait()?.is_none() {
                running.child.kill()?;
                let _ = running.child.wait();
            }
            let network_log_path = Self::network_log_path(handle);
            if let Some(proxy) = running.network_proxy.as_mut() {
                self.append_network_log_line(&network_log_path, "stopping host proxy")?;
                proxy.stop();
            }
            if let Some(lease) = running.network_lease.as_ref() {
                self.teardown_network_lease(lease, &network_log_path)?;
            }
        }

        if handle.run_dir.exists() {
            fs::remove_dir_all(&handle.run_dir)?;
        }

        Ok(())
    }
}

#[derive(Debug)]
struct PreparedRuntimeTree {
    image_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct FirecrackerConfigFile {
    #[serde(rename = "boot-source")]
    boot_source: BootSourceConfig,
    drives: Vec<DriveConfig>,
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
    #[serde(rename = "network-interfaces", skip_serializing_if = "Vec::is_empty")]
    network_interfaces: Vec<NetworkInterfaceConfig>,
    entropy: EntropyDeviceConfig,
}

#[derive(Debug, Serialize)]
struct BootSourceConfig {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Debug, Serialize)]
struct DriveConfig {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Debug, Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u32,
    smt: bool,
}

#[derive(Debug, Serialize)]
struct NetworkInterfaceConfig {
    iface_id: String,
    host_dev_name: String,
    guest_mac: String,
}

#[derive(Debug, Serialize)]
struct EntropyDeviceConfig {}

fn validate_firecracker_common(config: &FirecrackerBackendConfig) -> Result<(), SandboxError> {
    if config.guest_cid_base < 10_000 {
        return Err(SandboxError::InvalidConfig(
            "sandbox.firecracker.guest_cid_base must be at least 10000".to_string(),
        ));
    }
    if config.networking.tap_name_prefix.is_empty() {
        return Err(SandboxError::InvalidConfig(
            "sandbox.firecracker.network.tap_name_prefix must not be empty".to_string(),
        ));
    }
    parse_ipv4_cidr(&config.networking.subnet_cidr)?;

    Ok(())
}

fn validate_runtime_dir(path: &Path) -> Result<(), SandboxError> {
    fs::create_dir_all(path)?;
    let probe = path.join(".openoman-write-probe");
    fs::write(&probe, b"ok")?;
    fs::remove_file(probe)?;
    Ok(())
}

fn validate_regular_file(path: &Path, field_name: &str) -> Result<(), SandboxError> {
    if !path.exists() {
        return Err(SandboxError::MissingDependency(format!(
            "{field_name} does not exist: {}",
            path.display()
        )));
    }

    if !path.is_file() {
        return Err(SandboxError::InvalidConfig(format!(
            "{field_name} must point to a file: {}",
            path.display()
        )));
    }

    Ok(())
}

fn validate_user_package_dirs(package_dirs: &[UserPackageDir]) -> Result<(), SandboxError> {
    let mut guest_paths = std::collections::BTreeSet::new();
    for package_dir in package_dirs {
        if !package_dir.host_path.exists() {
            return Err(SandboxError::MissingDependency(format!(
                "sandbox.firecracker.user_package_dirs host_path does not exist: {}",
                package_dir.host_path.display()
            )));
        }

        if !package_dir.host_path.is_dir() {
            return Err(SandboxError::InvalidConfig(format!(
                "sandbox.firecracker.user_package_dirs host_path must be a directory: {}",
                package_dir.host_path.display()
            )));
        }

        if !package_dir.guest_path.is_absolute() {
            return Err(SandboxError::InvalidConfig(format!(
                "sandbox.firecracker.user_package_dirs guest_path must be absolute: {}",
                package_dir.guest_path.display()
            )));
        }

        if !guest_paths.insert(package_dir.guest_path.clone()) {
            return Err(SandboxError::InvalidConfig(format!(
                "duplicate sandbox.firecracker.user_package_dirs guest_path: {}",
                package_dir.guest_path.display()
            )));
        }
    }

    Ok(())
}

fn ensure_command_exists(program: &str) -> Result<PathBuf, SandboxError> {
    resolve_command_path(program).map_err(|_| {
        SandboxError::MissingDependency(format!("required command not found in PATH: {program}"))
    })
}

fn resolve_command_path(program: &str) -> Result<PathBuf, SandboxError> {
    let program_path = Path::new(program);
    if program_path.is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        if program_path.exists() {
            return Ok(program_path.to_path_buf());
        }
        return Err(SandboxError::MissingDependency(format!(
            "required command path does not exist: {program}"
        )));
    }

    let path_var = std::env::var_os("PATH").ok_or_else(|| {
        SandboxError::MissingDependency(format!("PATH is not set while resolving {program}"))
    })?;

    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(SandboxError::MissingDependency(format!(
        "required command not found in PATH: {program}"
    )))
}

fn compute_image_size_bytes(root: &Path, disk_quota_bytes: u64) -> Result<u64, SandboxError> {
    let source_size = directory_size_bytes(root)?;
    let minimum = 64 * 1024 * 1024_u64;
    let overhead = 128 * 1024 * 1024_u64;
    let requested = (source_size.saturating_add(overhead)).max(minimum);
    if requested > disk_quota_bytes {
        return Err(SandboxError::InvalidConfig(format!(
            "prepared runtime input needs {requested} bytes but disk quota is only {disk_quota_bytes} bytes"
        )));
    }
    Ok(requested)
}

fn directory_size_bytes(path: &Path) -> Result<u64, SandboxError> {
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

type SharedLog = Arc<Mutex<fs::File>>;

fn append_host_network_log(logs_path: &Path, network_log_path: &Path) -> Result<(), SandboxError> {
    let network_log = fs::read_to_string(network_log_path)?;
    if network_log.is_empty() {
        return Ok(());
    }

    let existing = fs::read(logs_path)?;
    let mut logs_file = fs::OpenOptions::new().append(true).open(logs_path)?;
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        writeln!(logs_file)?;
    }
    writeln!(logs_file)?;
    writeln!(logs_file, "[host network log]")?;
    write!(logs_file, "{network_log}")?;
    if !network_log.ends_with('\n') {
        writeln!(logs_file)?;
    }
    Ok(())
}

fn write_shared_log(log: &SharedLog, message: &str) {
    if let Ok(mut file) = log.lock() {
        let _ = writeln!(file, "{message}");
    }
}

fn handle_connect_proxy_client(
    mut client: TcpStream,
    peer_addr: String,
    allowed_domains: Arc<Vec<String>>,
    log: SharedLog,
) {
    let request = match read_connect_proxy_request(&mut client) {
        Ok(request) => request,
        Err(err) => {
            let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            write_shared_log(
                &log,
                &format!("proxy rejected malformed request from {peer_addr}: {err}"),
            );
            return;
        }
    };

    let (method, host, port) = match parse_connect_request(&request) {
        Ok(parts) => parts,
        Err(err) => {
            let status = if err.contains("CONNECT") {
                b"HTTP/1.1 405 Method Not Allowed\r\n\r\n".as_slice()
            } else {
                b"HTTP/1.1 400 Bad Request\r\n\r\n".as_slice()
            };
            let _ = client.write_all(status);
            write_shared_log(
                &log,
                &format!("proxy rejected bad request from {peer_addr}: {err}"),
            );
            return;
        }
    };

    if method != "CONNECT" {
        let _ = client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n");
        write_shared_log(
            &log,
            &format!("proxy rejected unsupported method from {peer_addr}: {method}"),
        );
        return;
    }

    if port != 443 {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        write_shared_log(
            &log,
            &format!("proxy denied {host}:{port} from {peer_addr}: only port 443 is allowed"),
        );
        return;
    }
    if !allowed_domains.iter().any(|allowed| allowed == &host) {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        write_shared_log(
            &log,
            &format!("proxy denied {host}:{port} from {peer_addr}: hostname not allowlisted"),
        );
        return;
    }

    match TcpStream::connect((host.as_str(), port)) {
        Ok(mut upstream) => {
            if client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .is_err()
            {
                write_shared_log(
                    &log,
                    &format!(
                        "proxy failed to acknowledge CONNECT for {host}:{port} from {peer_addr}"
                    ),
                );
                return;
            }
            write_shared_log(
                &log,
                &format!("proxy accepted {host}:{port} from {peer_addr}"),
            );
            if let Err(err) = tunnel_tcp_streams(client, &mut upstream) {
                write_shared_log(
                    &log,
                    &format!(
                        "proxy tunnel to {host}:{port} from {peer_addr} ended with error: {err}"
                    ),
                );
            } else {
                write_shared_log(
                    &log,
                    &format!("proxy tunnel to {host}:{port} from {peer_addr} closed cleanly"),
                );
            }
        }
        Err(err) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            write_shared_log(
                &log,
                &format!("proxy upstream connect failed for {host}:{port} from {peer_addr}: {err}"),
            );
        }
    }
}

fn read_connect_proxy_request(stream: &mut TcpStream) -> io::Result<String> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    while request.len() < PROXY_REQUEST_LIMIT_BYTES {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before proxy request headers were complete",
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&request).into_owned());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "proxy request headers exceeded size limit",
    ))
}

fn parse_connect_request(request: &str) -> Result<(String, String, u16), &'static str> {
    let first_line = request.lines().next().ok_or("empty request")?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().ok_or("missing HTTP method")?.to_string();
    let target = parts.next().ok_or("missing CONNECT target")?;
    let version = parts.next().ok_or("missing HTTP version")?;
    if !version.starts_with("HTTP/1.") {
        return Err("unsupported HTTP version");
    }
    let (host, port) = target
        .rsplit_once(':')
        .ok_or("CONNECT target must be host:port")?;
    let host = host
        .trim_matches(|ch| ch == '[' || ch == ']')
        .trim()
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("CONNECT target host must not be empty");
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| "CONNECT target port must be numeric")?;
    Ok((method, host, port))
}

fn tunnel_tcp_streams(client: TcpStream, upstream: &mut TcpStream) -> io::Result<()> {
    let mut client_reader = client.try_clone()?;
    let mut client_writer = client;
    let mut upstream_writer = upstream.try_clone()?;
    let upstream_to_client = thread::spawn(move || {
        let _ = io::copy(&mut client_reader, &mut upstream_writer);
        let _ = upstream_writer.shutdown(Shutdown::Write);
    });

    let mut upstream_reader = upstream.try_clone()?;
    let result = io::copy(&mut upstream_reader, &mut client_writer);
    let _ = client_writer.shutdown(Shutdown::Write);
    let _ = upstream_to_client.join();
    result.map(|_| ())
}

fn allocate_network_lease(
    subnet_cidr: &str,
    handle_id: u64,
    tap_name_prefix: &str,
    proxy_port: u16,
) -> Result<NetworkLease, SandboxError> {
    let (subnet_base, subnet_prefix) = parse_ipv4_cidr(subnet_cidr)?;
    let Some(handle_index) = handle_id.checked_sub(1) else {
        return Err(SandboxError::RunnerState(format!(
            "invalid firecracker handle id {handle_id} for network lease allocation"
        )));
    };
    let available_subnets = 1_u64 << u32::from(30 - subnet_prefix);
    if handle_index >= available_subnets {
        return Err(SandboxError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr {subnet_cidr} does not have enough /30 networks for handle id {handle_id}"
        )));
    }

    let block_base = subnet_base
        .checked_add(u32::try_from(handle_index * 4).map_err(|_| {
            SandboxError::RunnerState(format!(
                "network handle id {handle_id} overflowed /30 block allocation"
            ))
        })?)
        .ok_or_else(|| {
            SandboxError::RunnerState(format!(
                "network handle id {handle_id} overflowed IPv4 lease allocation"
            ))
        })?;
    let host_ip = u32_to_ipv4(block_base + 1);
    let guest_ip = u32_to_ipv4(block_base + 2);
    let mac_bytes = handle_id.to_be_bytes();
    let guest_mac = format!(
        "02:fc:{:02x}:{:02x}:{:02x}:{:02x}",
        mac_bytes[4], mac_bytes[5], mac_bytes[6], mac_bytes[7]
    );

    Ok(NetworkLease {
        tap_name: make_tap_name(tap_name_prefix, handle_id)?,
        guest_iface: "eth0".to_string(),
        host_ip,
        guest_ip,
        prefix_len: 30,
        guest_mac,
        proxy_port,
    })
}

fn parse_ipv4_cidr(raw: &str) -> Result<(u32, u8), SandboxError> {
    let (address, prefix) = raw.split_once('/').ok_or_else(|| {
        SandboxError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must be an IPv4 CIDR like 172.22.0.0/16: {raw}"
        ))
    })?;
    let address = address.parse::<Ipv4Addr>().map_err(|_| {
        SandboxError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must contain a valid IPv4 address: {raw}"
        ))
    })?;
    let prefix_len = prefix.parse::<u8>().map_err(|_| {
        SandboxError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must contain a numeric prefix length: {raw}"
        ))
    })?;
    if prefix_len > 30 {
        return Err(SandboxError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must have prefix length 30 or smaller: {raw}"
        )));
    }

    let address_u32 = ipv4_to_u32(address);
    let network_mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << u32::from(32 - prefix_len)
    };
    if address_u32 & !network_mask != 0 {
        return Err(SandboxError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must use a network base address aligned to its prefix: {raw}"
        )));
    }
    Ok((address_u32, prefix_len))
}

fn make_tap_name(prefix: &str, handle_id: u64) -> Result<String, SandboxError> {
    let suffix = handle_id.to_string();
    if suffix.len() >= 15 {
        return Err(SandboxError::InvalidConfig(format!(
            "firecracker handle id {handle_id} is too large to fit in a Linux tap interface name"
        )));
    }
    let max_prefix_len = 15 - suffix.len();
    let truncated_prefix = prefix.chars().take(max_prefix_len).collect::<String>();
    if truncated_prefix.is_empty() {
        return Err(SandboxError::InvalidConfig(
            "sandbox.firecracker.network.tap_name_prefix must leave room for a numeric suffix"
                .to_string(),
        ));
    }
    Ok(format!("{truncated_prefix}{suffix}"))
}

fn ipv4_to_u32(address: Ipv4Addr) -> u32 {
    u32::from_be_bytes(address.octets())
}

fn u32_to_ipv4(address: u32) -> Ipv4Addr {
    Ipv4Addr::from(address.to_be_bytes())
}

fn render_agent_env(
    agent: &AgentExecutionSpec,
    guest_path_entries: &[String],
    user_package_dirs: &[UserPackageDir],
    network_lease: Option<&NetworkLease>,
) -> String {
    let mut env_file = String::new();
    env_file.push_str(&format!(
        "AGENT_PROVIDER={}\n",
        match agent.provider {
            AgentProvider::Codex => "codex",
        }
    ));
    env_file.push_str(&format!(
        "CODEX_BIN={}\n",
        shell_quote(&resolve_guest_codex_bin(
            &agent.codex_bin,
            user_package_dirs
        ))
    ));
    if agent.codex_auth_file.is_some() {
        env_file
            .push_str("OPENOMAN_CODEX_AUTH_FILE='/mnt/runtime/openoman-config/codex-auth.json'\n");
    }
    if let Some(proxy) = &agent.egress_proxy {
        env_file.push_str(&format!("HTTPS_PROXY={}\n", shell_quote(proxy)));
        env_file.push_str(&format!("HTTP_PROXY={}\n", shell_quote(proxy)));
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
            "HTTPS_PROXY={}\n",
            shell_quote(&lease.host_proxy_url())
        ));
        env_file.push_str(&format!(
            "HTTP_PROXY={}\n",
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

fn resolve_guest_codex_bin(codex_bin: &str, user_package_dirs: &[UserPackageDir]) -> String {
    let codex_path = Path::new(codex_bin);
    if codex_path.is_absolute() {
        for package_dir in user_package_dirs {
            if let Ok(relative) = codex_path.strip_prefix(&package_dir.host_path) {
                return package_dir.guest_path.join(relative).display().to_string();
            }
        }
    }

    codex_bin.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn base_runtime_config(root: &Path, firecracker_bin: &Path) -> SandboxRuntimeConfig {
        let kernel = root.join("vmlinux");
        let rootfs = root.join("rootfs.ext4");
        fs::write(&kernel, "kernel").expect("write kernel");
        fs::write(&rootfs, "rootfs").expect("write rootfs");

        SandboxRuntimeConfig {
            backend: SandboxBackendKind::Firecracker,
            runtime_dir: root.join("sandbox-runtime"),
            limits: super::super::ResourceLimits {
                vcpu_count: 1,
                memory_mib: 256,
                disk_quota_bytes: 256 * 1024 * 1024,
                timeout_secs: 5,
            },
            firecracker: Some(FirecrackerBackendConfig {
                mode: FirecrackerMode::Direct,
                firecracker_bin: firecracker_bin.display().to_string(),
                jailer_bin: "jailer".to_string(),
                kernel_image_path: kernel,
                rootfs_image_path: rootfs,
                guest_cid_base: 10_000,
                user_package_dirs: Vec::new(),
                networking: super::super::FirecrackerNetworkingConfig {
                    mode: super::super::FirecrackerNetworkingMode::Disabled,
                    privilege_mode: super::super::FirecrackerNetworkPrivilegeMode::Direct,
                    tap_name_prefix: "oomtap".to_string(),
                    proxy_port: 3128,
                    subnet_cidr: "172.22.0.0/16".to_string(),
                },
            }),
        }
    }

    #[test]
    fn build_backend_rejects_duplicate_guest_paths() {
        let temp = TempDir::new().expect("tempdir");
        let fake_firecracker = write_fake_firecracker(temp.path());
        let mut config = base_runtime_config(temp.path(), &fake_firecracker);
        config
            .firecracker
            .as_mut()
            .expect("firecracker config")
            .user_package_dirs = vec![
            UserPackageDir {
                host_path: temp.path().to_path_buf(),
                guest_path: PathBuf::from("/opt/tools"),
                add_to_path: true,
            },
            UserPackageDir {
                host_path: temp.path().to_path_buf(),
                guest_path: PathBuf::from("/opt/tools"),
                add_to_path: false,
            },
        ];

        let backend = FirecrackerBackend::new(config).expect("backend");
        let err = backend
            .check_runtime_dependencies()
            .expect_err("duplicate guest paths should fail");
        assert!(err
            .to_string()
            .contains("duplicate sandbox.firecracker.user_package_dirs guest_path"));
    }

    #[test]
    fn build_backend_rejects_jailer_mode_for_now() {
        let temp = TempDir::new().expect("tempdir");
        let fake_firecracker = write_fake_firecracker(temp.path());
        let mut config = base_runtime_config(temp.path(), &fake_firecracker);
        let firecracker = config.firecracker.as_mut().expect("firecracker config");
        firecracker.mode = FirecrackerMode::Jailer;
        firecracker.jailer_bin = fake_firecracker.display().to_string();

        let backend = FirecrackerBackend::new(config).expect("backend");
        let err = backend
            .check_runtime_dependencies()
            .expect_err("jailer mode should fail");
        assert!(matches!(err, SandboxError::NotImplemented(_)));
    }

    #[test]
    fn stage_runtime_tree_creates_image_with_requested_size() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");

        let fake_firecracker = write_fake_firecracker(temp.path());
        let config = base_runtime_config(temp.path(), &fake_firecracker);
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker
                .clone()
                .expect("firecracker config should exist"),
        );
        let run_dir = config.runtime_dir.join("runs").join("job-size-attempt-1-1");
        fs::create_dir_all(&run_dir).expect("create run dir");

        let prepared = runner
            .stage_runtime_tree(
                &AttemptSpec {
                    job_id: "job-size".to_string(),
                    attempt_id: 1,
                    workspace_dir: workspace,
                    instruction: "noop".to_string(),
                    limits: config.limits.clone(),
                    agent: AgentExecutionSpec {
                        provider: AgentProvider::Codex,
                        codex_bin: "codex".to_string(),
                        codex_auth_file: None,
                        egress_proxy: None,
                        egress_allowed_domains: Vec::new(),
                    },
                },
                &run_dir,
                None,
            )
            .expect("stage runtime tree");

        let requested_size = compute_image_size_bytes(
            &run_dir.join("runtime-tree"),
            config.limits.disk_quota_bytes,
        )
        .expect("compute requested size");
        let expected_image_size =
            requested_size.div_ceil(EXT4_BLOCK_SIZE_BYTES) * EXT4_BLOCK_SIZE_BYTES;
        let actual_image_size = fs::metadata(&prepared.image_path)
            .expect("runtime image metadata")
            .len();

        assert_eq!(actual_image_size, expected_image_size);
    }

    #[test]
    fn stage_runtime_tree_copies_codex_auth_file_when_configured() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let auth_file = temp.path().join("auth.json");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");
        fs::write(&auth_file, "{\"auth_mode\":\"chatgpt\"}\n").expect("write auth file");

        let fake_firecracker = write_fake_firecracker(temp.path());
        let config = base_runtime_config(temp.path(), &fake_firecracker);
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker
                .clone()
                .expect("firecracker config should exist"),
        );
        let run_dir = config.runtime_dir.join("runs").join("job-auth-attempt-1-1");
        fs::create_dir_all(&run_dir).expect("create run dir");

        let prepared = runner
            .stage_runtime_tree(
                &AttemptSpec {
                    job_id: "job-auth".to_string(),
                    attempt_id: 1,
                    workspace_dir: workspace,
                    instruction: "noop".to_string(),
                    limits: config.limits.clone(),
                    agent: AgentExecutionSpec {
                        provider: AgentProvider::Codex,
                        codex_bin: "codex".to_string(),
                        codex_auth_file: Some(auth_file),
                        egress_proxy: None,
                        egress_allowed_domains: Vec::new(),
                    },
                },
                &run_dir,
                None,
            )
            .expect("stage runtime tree");

        let auth_contents = Command::new("debugfs")
            .args([
                "-R",
                "cat /openoman-config/codex-auth.json",
                prepared.image_path.to_string_lossy().as_ref(),
            ])
            .output()
            .expect("read auth file from image");
        assert!(auth_contents.status.success());
        assert!(
            String::from_utf8_lossy(&auth_contents.stdout).contains("\"auth_mode\":\"chatgpt\"")
        );
    }

    #[test]
    fn direct_runner_collects_workspace_report_and_logs() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let fake_firecracker = write_fake_firecracker(temp.path());
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");

        let config = base_runtime_config(temp.path(), &fake_firecracker);
        let backend = FirecrackerBackend::new(config.clone()).expect("backend");
        backend
            .check_runtime_dependencies()
            .expect("dependencies available");

        let mut runner = backend.create_runner().expect("runner");
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-1".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "append blank line to readme".to_string(),
                limits: config.limits.clone(),
                agent: AgentExecutionSpec {
                    provider: AgentProvider::Codex,
                    codex_bin: "codex".to_string(),
                    codex_auth_file: None,
                    egress_proxy: Some("http://proxy.internal:3128".to_string()),
                    egress_allowed_domains: vec!["api.openai.com".to_string()],
                },
            })
            .expect("start");

        let status = runner.wait(&handle).expect("wait");
        assert!(status.success);
        let output = runner
            .collect_output(&handle, "job-1", 1)
            .expect("collect output");

        assert!(output.modified_workspace_dir.exists());
        assert!(output.report_path.exists());
        assert!(output.logs_path.exists());
        assert_eq!(
            fs::read_to_string(output.modified_workspace_dir.join("README.md")).expect("readme"),
            "hello\n\n"
        );

        let report = fs::read_to_string(&output.report_path).expect("report");
        assert!(report.contains("fake firecracker completed"));
        let logs = fs::read_to_string(&output.logs_path).expect("logs");
        assert!(logs.contains("instruction: append blank line to readme"));

        runner.stop(&handle).expect("stop");
        assert!(!handle.run_dir.exists());
    }

    #[test]
    fn firecracker_config_includes_entropy_device() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let runtime_image = temp.path().join("runtime.ext4");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");
        fs::write(&runtime_image, "runtime").expect("write runtime image");

        let fake_firecracker = write_fake_firecracker(temp.path());
        let config = base_runtime_config(temp.path(), &fake_firecracker);
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker
                .clone()
                .expect("firecracker config should exist"),
        );
        let run_dir = config
            .runtime_dir
            .join("runs")
            .join("job-config-attempt-1-1");
        fs::create_dir_all(&run_dir).expect("create run dir");

        let config_path = runner
            .write_firecracker_config(
                &run_dir,
                &runner.firecracker.rootfs_image_path,
                &runtime_image,
                &AttemptSpec {
                    job_id: "job-config".to_string(),
                    attempt_id: 1,
                    workspace_dir: workspace,
                    instruction: "noop".to_string(),
                    limits: config.limits.clone(),
                    agent: AgentExecutionSpec {
                        provider: AgentProvider::Codex,
                        codex_bin: "codex".to_string(),
                        codex_auth_file: None,
                        egress_proxy: None,
                        egress_allowed_domains: Vec::new(),
                    },
                },
                None,
            )
            .expect("write firecracker config");

        let config_json: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).expect("read firecracker config"))
                .expect("parse firecracker config");

        assert_eq!(config_json["entropy"], serde_json::json!({}));
        assert!(config_json.get("network-interfaces").is_none());
    }

    #[test]
    fn firecracker_config_includes_network_interface_when_enabled() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let runtime_image = temp.path().join("runtime.ext4");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");
        fs::write(&runtime_image, "runtime").expect("write runtime image");

        let fake_firecracker = write_fake_firecracker(temp.path());
        let mut config = base_runtime_config(temp.path(), &fake_firecracker);
        config
            .firecracker
            .as_mut()
            .expect("firecracker config")
            .networking
            .mode = super::super::FirecrackerNetworkingMode::HostProxy;
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker
                .clone()
                .expect("firecracker config should exist"),
        );
        let run_dir = config
            .runtime_dir
            .join("runs")
            .join("job-network-config-attempt-1-1");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let lease =
            allocate_network_lease("172.22.0.0/16", 1, "oomtap", 3128).expect("allocate lease");

        let config_path = runner
            .write_firecracker_config(
                &run_dir,
                &runner.firecracker.rootfs_image_path,
                &runtime_image,
                &AttemptSpec {
                    job_id: "job-config".to_string(),
                    attempt_id: 1,
                    workspace_dir: workspace,
                    instruction: "noop".to_string(),
                    limits: config.limits.clone(),
                    agent: AgentExecutionSpec {
                        provider: AgentProvider::Codex,
                        codex_bin: "codex".to_string(),
                        codex_auth_file: None,
                        egress_proxy: None,
                        egress_allowed_domains: vec!["api.openai.com".to_string()],
                    },
                },
                Some(&lease),
            )
            .expect("write firecracker config");

        let config_json: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).expect("read firecracker config"))
                .expect("parse firecracker config");
        assert_eq!(config_json["network-interfaces"][0]["iface_id"], "eth0");
        assert_eq!(
            config_json["network-interfaces"][0]["host_dev_name"],
            "oomtap1"
        );
        assert_eq!(
            config_json["network-interfaces"][0]["guest_mac"],
            serde_json::json!("02:fc:00:00:00:01")
        );
    }

    #[test]
    fn allocate_network_lease_is_deterministic() {
        let lease =
            allocate_network_lease("172.22.0.0/16", 3, "oomtap", 3128).expect("allocate lease");
        assert_eq!(lease.tap_name, "oomtap3");
        assert_eq!(lease.host_ip, Ipv4Addr::new(172, 22, 0, 9));
        assert_eq!(lease.guest_ip, Ipv4Addr::new(172, 22, 0, 10));
        assert_eq!(lease.guest_ip_cidr(), "172.22.0.10/30");
        assert_eq!(lease.host_proxy_url(), "http://172.22.0.9:3128");
    }

    #[test]
    fn host_proxy_rejects_non_allowlisted_domain() {
        let temp = TempDir::new().expect("tempdir");
        let fake_firecracker = write_fake_firecracker(temp.path());
        let config = base_runtime_config(temp.path(), &fake_firecracker);
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker
                .clone()
                .expect("firecracker config should exist"),
        );
        let log_path = temp.path().join("network.log");
        let lease = NetworkLease {
            tap_name: "oomtap1".to_string(),
            guest_iface: "eth0".to_string(),
            host_ip: Ipv4Addr::LOCALHOST,
            guest_ip: Ipv4Addr::new(127, 0, 0, 2),
            prefix_len: 30,
            guest_mac: "02:fc:00:00:00:01".to_string(),
            proxy_port: 0,
        };
        let mut proxy = runner
            .start_network_proxy(&lease, &["api.openai.com".to_string()], &log_path)
            .expect("start proxy");

        let mut client = TcpStream::connect(proxy.bind_addr).expect("connect");
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .expect("write request");
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("read proxy response");
        proxy.stop();

        assert!(response.starts_with("HTTP/1.1 403 Forbidden"));
        let log_contents = fs::read_to_string(&log_path).expect("read network log");
        assert!(log_contents.contains("hostname not allowlisted"));
    }

    #[test]
    fn direct_runner_smoke_boots_real_firecracker_when_opted_in() {
        if std::env::var("OPENOMAN_FIRECRACKER_E2E").ok().as_deref() != Some("1") {
            return;
        }

        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let package_dir = temp.path().join("user-bin");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&package_dir).expect("create package dir");
        fs::write(workspace.join("README.md"), "hello from real firecracker\n")
            .expect("write workspace file");

        let fake_codex = write_real_guest_codex(package_dir.as_path());
        let runtime_config = real_firecracker_runtime_config(temp.path(), &package_dir);
        let backend = FirecrackerBackend::new(runtime_config.clone()).expect("backend");
        backend
            .check_runtime_dependencies()
            .expect("real firecracker dependencies available");

        let mut runner = backend.create_runner().expect("runner");
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-real".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "append blank line to readme".to_string(),
                limits: runtime_config.limits.clone(),
                agent: AgentExecutionSpec {
                    provider: AgentProvider::Codex,
                    codex_bin: fake_codex
                        .file_name()
                        .expect("codex filename")
                        .to_string_lossy()
                        .into_owned(),
                    codex_auth_file: None,
                    egress_proxy: None,
                    egress_allowed_domains: Vec::new(),
                },
            })
            .expect("start");
        let serial_log_path = handle.run_dir.join("serial.log");
        let vmm_log_path = handle.run_dir.join("firecracker.log");

        let status = runner.wait(&handle).expect("wait");
        let collected = runner
            .collect_output(&handle, "job-real", 1)
            .expect("collect output");
        let stop_result = runner.stop(&handle);
        let serial_output = fs::read_to_string(&serial_log_path).unwrap_or_default();
        let vmm_output = fs::read_to_string(&vmm_log_path).unwrap_or_default();
        let readme_after = fs::read_to_string(collected.modified_workspace_dir.join("README.md"))
            .expect("readme after guest");
        let report = fs::read_to_string(&collected.report_path).expect("report");
        let logs = fs::read_to_string(&collected.logs_path).expect("logs");

        assert!(
            status.success,
            "real firecracker guest failed with code {:?}\nvmm log:\n{}\nserial log:\n{}\nguest logs:\n{}",
            status.code,
            vmm_output,
            serial_output,
            logs
        );
        assert!(
            readme_after == "hello from real firecracker\n\n",
            "workspace was not modified as expected\nreport:\n{}\nlogs:\n{}\nreadme:\n{}",
            report,
            logs,
            readme_after
        );
        assert!(
            logs.contains("real-guest-codex finished"),
            "guest logs did not show codex completion\nreport:\n{}\nlogs:\n{}",
            report,
            logs
        );
        assert!(report.contains("real guest codex completed"));
        assert!(logs.contains("openoman guest init started"));
        assert!(logs.contains("real-guest-codex"));

        stop_result.expect("stop");
    }

    #[test]
    fn render_agent_env_includes_allowed_domains() {
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Codex,
                codex_bin: "/usr/local/bin/codex".to_string(),
                codex_auth_file: None,
                egress_proxy: Some("http://proxy.internal:3128".to_string()),
                egress_allowed_domains: vec![
                    "api.openai.com".to_string(),
                    "files.openai.com".to_string(),
                ],
            },
            &["/opt/openoman/user-bin".to_string()],
            &[],
            None,
        );

        assert!(env_file.contains("HTTPS_PROXY='http://proxy.internal:3128'"));
        assert!(env_file.contains("HTTP_PROXY='http://proxy.internal:3128'"));
        assert!(
            env_file.contains("OPENOMAN_EGRESS_ALLOWED_DOMAINS='api.openai.com,files.openai.com'")
        );
    }

    #[test]
    fn render_agent_env_includes_host_proxy_networking() {
        let lease =
            allocate_network_lease("172.22.0.0/16", 1, "oomtap", 3128).expect("allocate lease");
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Codex,
                codex_bin: "/usr/local/bin/codex".to_string(),
                codex_auth_file: None,
                egress_proxy: None,
                egress_allowed_domains: vec!["api.openai.com".to_string()],
            },
            &[],
            &[],
            Some(&lease),
        );

        assert!(env_file.contains("OPENOMAN_NET_MODE='host-proxy'"));
        assert!(env_file.contains("OPENOMAN_NET_IFACE='eth0'"));
        assert!(env_file.contains("OPENOMAN_NET_GUEST_IPV4='172.22.0.2/30'"));
        assert!(env_file.contains("OPENOMAN_NET_HOST_PROXY_URL='http://172.22.0.1:3128'"));
        assert!(env_file.contains("HTTPS_PROXY='http://172.22.0.1:3128'"));
        assert!(env_file.contains("HTTP_PROXY='http://172.22.0.1:3128'"));
    }

    #[test]
    fn render_agent_env_includes_codex_auth_file_when_configured() {
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Codex,
                codex_bin: "/usr/local/bin/codex".to_string(),
                codex_auth_file: Some(PathBuf::from("/host/.codex/auth.json")),
                egress_proxy: None,
                egress_allowed_domains: Vec::new(),
            },
            &[],
            &[],
            None,
        );

        assert!(env_file
            .contains("OPENOMAN_CODEX_AUTH_FILE='/mnt/runtime/openoman-config/codex-auth.json'"));
    }

    fn write_fake_firecracker(root: &Path) -> PathBuf {
        let script_path = root.join("fake-firecracker.sh");
        fs::write(
            &script_path,
            r#"#!/usr/bin/env sh
set -eu
image="${OPENOMAN_FAKE_RUNTIME_IMAGE:?}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
debugfs -R "dump -p /openoman-config/instruction.txt $tmpdir/instruction.txt" "$image" >/dev/null 2>&1
instruction="$(cat "$tmpdir/instruction.txt")"
debugfs -R "dump -p /workspace/README.md $tmpdir/README.md" "$image" >/dev/null 2>&1 || true
if printf "%s" "$instruction" | grep -qi "readme"; then
  printf "\n" >> "$tmpdir/README.md"
  debugfs -w -R "rm /workspace/README.md" "$image" >/dev/null 2>&1 || true
  debugfs -w -R "write $tmpdir/README.md /workspace/README.md" "$image" >/dev/null 2>&1
else
  printf "agent touched workspace\n" > "$tmpdir/AGENT_OUTPUT.txt"
  debugfs -w -R "write $tmpdir/AGENT_OUTPUT.txt /workspace/AGENT_OUTPUT.txt" "$image" >/dev/null 2>&1
fi
printf "instruction: %s\n" "$instruction" > "$tmpdir/logs.txt"
printf "fake firecracker completed: %s\n" "$instruction" > "$tmpdir/report.txt"
debugfs -w -R "write $tmpdir/logs.txt /openoman-output/logs.txt" "$image" >/dev/null 2>&1
debugfs -w -R "write $tmpdir/report.txt /openoman-output/report.txt" "$image" >/dev/null 2>&1
if printf "%s" "$instruction" | grep -qi "fail"; then
  exit 9
fi
exit 0
"#,
        )
        .expect("write fake firecracker");
        fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod fake firecracker");
        script_path
    }

    fn real_firecracker_runtime_config(root: &Path, package_dir: &Path) -> SandboxRuntimeConfig {
        SandboxRuntimeConfig {
            backend: SandboxBackendKind::Firecracker,
            runtime_dir: root.join("sandbox-runtime"),
            limits: super::super::ResourceLimits {
                vcpu_count: 1,
                memory_mib: 512,
                disk_quota_bytes: 512 * 1024 * 1024,
                timeout_secs: 30,
            },
            firecracker: Some(FirecrackerBackendConfig {
                mode: FirecrackerMode::Direct,
                firecracker_bin: real_firecracker_bin(),
                jailer_bin: "jailer".to_string(),
                kernel_image_path: real_firecracker_kernel_path(),
                rootfs_image_path: real_firecracker_rootfs_path(),
                guest_cid_base: 10_000,
                user_package_dirs: vec![UserPackageDir {
                    host_path: package_dir.to_path_buf(),
                    guest_path: PathBuf::from("/opt/openoman/user-bin"),
                    add_to_path: true,
                }],
                networking: super::super::FirecrackerNetworkingConfig {
                    mode: super::super::FirecrackerNetworkingMode::Disabled,
                    privilege_mode: super::super::FirecrackerNetworkPrivilegeMode::Direct,
                    tap_name_prefix: "oomtap".to_string(),
                    proxy_port: 3128,
                    subnet_cidr: "172.22.0.0/16".to_string(),
                },
            }),
        }
    }

    fn real_firecracker_bin() -> String {
        if let Ok(path) = std::env::var("OPENOMAN_FIRECRACKER_BIN") {
            return path;
        }

        if let Ok(home) = std::env::var("HOME") {
            let local = PathBuf::from(home).join(".local/bin/firecracker");
            if local.exists() {
                return local.display().to_string();
            }
        }

        "firecracker".to_string()
    }

    fn real_firecracker_kernel_path() -> PathBuf {
        if let Ok(path) = std::env::var("OPENOMAN_FIRECRACKER_KERNEL") {
            return PathBuf::from(path);
        }

        repo_root().join("guest/out/vmlinux")
    }

    fn real_firecracker_rootfs_path() -> PathBuf {
        if let Ok(path) = std::env::var("OPENOMAN_FIRECRACKER_ROOTFS") {
            return PathBuf::from(path);
        }

        repo_root().join("guest/out/rootfs.ext4")
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("canonicalize repo root")
    }

    fn write_real_guest_codex(root: &Path) -> PathBuf {
        let script_path = root.join("codex");
        fs::write(
            &script_path,
            r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "real-guest-codex 0.0-test"
  exit 0
fi
if [ "${1:-}" != "exec" ]; then
  echo "unexpected invocation: $*" >&2
  exit 2
fi
shift
workdir="."
report=""
instruction=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -C)
      workdir="$2"
      shift 2
      ;;
    -o)
      report="$2"
      shift 2
      ;;
    --full-auto)
      shift 1
      ;;
    --color)
      shift 2
      ;;
    *)
      instruction="$1"
      shift 1
      ;;
  esac
done
cd "$workdir"
printf "\n" >> README.md
printf "real guest codex completed: %s\n" "$instruction" > "$report"
echo "real-guest-codex finished"
"#,
        )
        .expect("write guest codex");
        fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod guest codex");
        script_path
    }
}
