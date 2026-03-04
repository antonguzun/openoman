mod artifacts;
mod launch;
mod network;
mod runtime;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Child,
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpStream},
    process::Command,
};

#[cfg(test)]
use super::{AgentExecutionSpec, AgentProvider};

use super::{
    AttemptSpec, CollectedExecutionOutput, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionError, ExecutionExitStatus, ExecutionHandle, ExecutionIsolation,
    ExecutionRunner, ExecutionRuntimeConfig, FirecrackerBackendConfig, FirecrackerMode,
    FirecrackerNetworkPrivilegeMode, FirecrackerNetworkingMode, UserPackageDir,
};
use artifacts::FirecrackerArtifactCollector;
use launch::FirecrackerVmLauncher;
use network::{parse_ipv4_cidr, FirecrackerNetworkController, NetworkLease, NetworkProxyHandle};
use runtime::{FirecrackerRuntimeStager, PreparedRuntimeTree};

#[cfg(test)]
use network::allocate_network_lease;

#[cfg(test)]
use runtime::{compute_image_size_bytes, render_agent_env};

const EXT4_BLOCK_SIZE_BYTES: u64 = 4096;
const FIRECRACKER_NET_HELPER_SUBCOMMAND: &[&str] = &["internal", "firecracker-net"];
const PROXY_REQUEST_LIMIT_BYTES: usize = 16 * 1024;

#[derive(Debug)]
pub struct FirecrackerBackend {
    runtime: ExecutionRuntimeConfig,
    firecracker: FirecrackerBackendConfig,
}

impl FirecrackerBackend {
    pub fn new(runtime: ExecutionRuntimeConfig) -> Result<Self, ExecutionError> {
        let Some(firecracker) = runtime.firecracker().cloned() else {
            return Err(ExecutionError::InvalidConfig(
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

impl ExecutionBackend for FirecrackerBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Firecracker
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities {
            isolation: ExecutionIsolation::MicroVm,
            requires_explicit_risk_acknowledgement: false,
            supports_host_proxy_networking: true,
        }
    }

    fn check_runtime_dependencies(&self) -> Result<(), ExecutionError> {
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
                    return Err(ExecutionError::MissingDependency(
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
                Err(ExecutionError::NotImplemented(
                    "sandbox backend firecracker(jailer) needs additional privileged environment preparation; use sandbox.firecracker.mode = \"direct\" in this release".to_string(),
                ))
            }
        }
    }

    fn create_runner(&self) -> Result<Box<dyn ExecutionRunner>, ExecutionError> {
        match self.firecracker.mode {
            FirecrackerMode::Direct => Ok(Box::new(FirecrackerDirectRunner::new(
                self.runtime.runtime_dir.clone(),
                self.firecracker.clone(),
            ))),
            FirecrackerMode::Jailer => Err(ExecutionError::NotImplemented(
                "sandbox backend firecracker(jailer) needs additional privileged environment preparation; use sandbox.firecracker.mode = \"direct\" in this release".to_string(),
            )),
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

    fn stage_runtime_tree(
        &self,
        spec: &AttemptSpec,
        run_dir: &Path,
        network_lease: Option<&NetworkLease>,
    ) -> Result<PreparedRuntimeTree, ExecutionError> {
        FirecrackerRuntimeStager::new(&self.firecracker).stage_runtime_tree(
            spec,
            run_dir,
            network_lease,
        )
    }

    fn write_firecracker_config(
        &self,
        run_dir: &Path,
        rootfs_image_path: &Path,
        runtime_image_path: &Path,
        spec: &AttemptSpec,
        network_lease: Option<&NetworkLease>,
    ) -> Result<PathBuf, ExecutionError> {
        FirecrackerVmLauncher::new(&self.firecracker).write_firecracker_config(
            run_dir,
            rootfs_image_path,
            runtime_image_path,
            spec,
            network_lease,
        )
    }

    fn runtime_image_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("runtime.ext4")
    }

    fn rootfs_copy_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("rootfs.ext4")
    }

    fn serial_log_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("serial.log")
    }

    fn vmm_log_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("firecracker.log")
    }

    fn network_log_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("network.log")
    }

    fn read_exit_code_marker(&self, image_path: &Path) -> Result<Option<i32>, ExecutionError> {
        FirecrackerArtifactCollector::new().read_exit_code_marker(image_path)
    }

    fn configure_networking(
        &self,
        handle: &ExecutionHandle,
        spec: &AttemptSpec,
    ) -> Result<(Option<NetworkLease>, Option<NetworkProxyHandle>), ExecutionError> {
        let log_path = Self::network_log_path(handle);
        FirecrackerNetworkController::new(&self.firecracker)
            .configure_networking(handle, spec, &log_path)
    }

    fn append_network_log_line(
        &self,
        log_path: &Path,
        message: &str,
    ) -> Result<(), ExecutionError> {
        FirecrackerNetworkController::new(&self.firecracker)
            .append_network_log_line(log_path, message)
    }

    #[cfg(test)]
    fn start_network_proxy(
        &self,
        lease: &NetworkLease,
        allowed_domains: &[String],
        log_path: &Path,
    ) -> Result<NetworkProxyHandle, ExecutionError> {
        FirecrackerNetworkController::new(&self.firecracker).start_network_proxy(
            lease,
            allowed_domains,
            log_path,
        )
    }

    fn teardown_network_lease(
        &self,
        lease: &NetworkLease,
        log_path: &Path,
    ) -> Result<(), ExecutionError> {
        FirecrackerNetworkController::new(&self.firecracker).teardown_network_lease(lease, log_path)
    }
}

impl ExecutionRunner for FirecrackerDirectRunner {
    fn start(&mut self, spec: AttemptSpec) -> Result<ExecutionHandle, ExecutionError> {
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
        let handle = ExecutionHandle {
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
        let vmm_log = Self::vmm_log_path(&handle);

        let child = match FirecrackerVmLauncher::new(&self.firecracker).spawn_firecracker_process(
            handle_id,
            &spec.job_id,
            &prepared.image_path,
            &config_path,
            &Self::serial_log_path(&handle),
            &vmm_log,
        ) {
            Ok(child) => child,
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

    fn wait(&mut self, handle: &ExecutionHandle) -> Result<ExecutionExitStatus, ExecutionError> {
        if !self.running.contains_key(&handle.id) {
            return Err(ExecutionError::RunnerState(format!(
                "unknown handle {}",
                handle.id
            )));
        }
        let start = Instant::now();
        let image_path = Self::runtime_image_path(handle);

        loop {
            let status = {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    ExecutionError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.try_wait()?
            };
            if let Some(status) = status {
                return Ok(status.into());
            }

            if let Some(code) = self.read_exit_code_marker(&image_path)? {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    ExecutionError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.kill()?;
                let _ = running.child.wait();
                return Ok(ExecutionExitStatus {
                    success: code == 0,
                    code: Some(code),
                    timed_out: false,
                });
            }

            let timeout = self
                .running
                .get(&handle.id)
                .ok_or_else(|| {
                    ExecutionError::RunnerState(format!("unknown handle {}", handle.id))
                })?
                .timeout;
            if start.elapsed() >= timeout {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    ExecutionError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.kill()?;
                let status = running.child.wait()?;
                return Ok(ExecutionExitStatus {
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
        handle: &ExecutionHandle,
        job_id: &str,
        attempt_id: u32,
    ) -> Result<CollectedExecutionOutput, ExecutionError> {
        let output_dir = self.stable_output_dir(job_id, attempt_id);
        FirecrackerArtifactCollector::new().collect_output(
            &Self::runtime_image_path(handle),
            &output_dir,
            &Self::serial_log_path(handle),
            &Self::network_log_path(handle),
        )
    }

    fn stop(&mut self, handle: &ExecutionHandle) -> Result<(), ExecutionError> {
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

fn validate_firecracker_common(config: &FirecrackerBackendConfig) -> Result<(), ExecutionError> {
    if config.guest_cid_base < 10_000 {
        return Err(ExecutionError::InvalidConfig(
            "sandbox.firecracker.guest_cid_base must be at least 10000".to_string(),
        ));
    }
    if config.networking.tap_name_prefix.is_empty() {
        return Err(ExecutionError::InvalidConfig(
            "sandbox.firecracker.network.tap_name_prefix must not be empty".to_string(),
        ));
    }
    parse_ipv4_cidr(&config.networking.subnet_cidr)?;

    Ok(())
}

fn validate_runtime_dir(path: &Path) -> Result<(), ExecutionError> {
    fs::create_dir_all(path)?;
    let probe = path.join(".openoman-write-probe");
    fs::write(&probe, b"ok")?;
    fs::remove_file(probe)?;
    Ok(())
}

fn validate_regular_file(path: &Path, field_name: &str) -> Result<(), ExecutionError> {
    if !path.exists() {
        return Err(ExecutionError::MissingDependency(format!(
            "{field_name} does not exist: {}",
            path.display()
        )));
    }

    if !path.is_file() {
        return Err(ExecutionError::InvalidConfig(format!(
            "{field_name} must point to a file: {}",
            path.display()
        )));
    }

    Ok(())
}

fn validate_user_package_dirs(package_dirs: &[UserPackageDir]) -> Result<(), ExecutionError> {
    let mut guest_paths = std::collections::BTreeSet::new();
    for package_dir in package_dirs {
        if !package_dir.host_path.exists() {
            return Err(ExecutionError::MissingDependency(format!(
                "sandbox.firecracker.user_package_dirs host_path does not exist: {}",
                package_dir.host_path.display()
            )));
        }

        if !package_dir.host_path.is_dir() {
            return Err(ExecutionError::InvalidConfig(format!(
                "sandbox.firecracker.user_package_dirs host_path must be a directory: {}",
                package_dir.host_path.display()
            )));
        }

        if !package_dir.guest_path.is_absolute() {
            return Err(ExecutionError::InvalidConfig(format!(
                "sandbox.firecracker.user_package_dirs guest_path must be absolute: {}",
                package_dir.guest_path.display()
            )));
        }

        if !guest_paths.insert(package_dir.guest_path.clone()) {
            return Err(ExecutionError::InvalidConfig(format!(
                "duplicate sandbox.firecracker.user_package_dirs guest_path: {}",
                package_dir.guest_path.display()
            )));
        }
    }

    Ok(())
}

fn ensure_command_exists(program: &str) -> Result<PathBuf, ExecutionError> {
    resolve_command_path(program).map_err(|_| {
        ExecutionError::MissingDependency(format!("required command not found in PATH: {program}"))
    })
}

fn resolve_command_path(program: &str) -> Result<PathBuf, ExecutionError> {
    let program_path = Path::new(program);
    if program_path.is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        if program_path.exists() {
            return Ok(program_path.to_path_buf());
        }
        return Err(ExecutionError::MissingDependency(format!(
            "required command path does not exist: {program}"
        )));
    }

    let path_var = std::env::var_os("PATH").ok_or_else(|| {
        ExecutionError::MissingDependency(format!("PATH is not set while resolving {program}"))
    })?;

    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(ExecutionError::MissingDependency(format!(
        "required command not found in PATH: {program}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        ExecutionBackendConfig, FirecrackerNetworkingConfig, HostRiskPosture, ResourceLimits,
    };
    use tempfile::TempDir;

    fn base_runtime_config(root: &Path, firecracker_bin: &Path) -> ExecutionRuntimeConfig {
        let kernel = root.join("vmlinux");
        let rootfs = root.join("rootfs.ext4");
        fs::write(&kernel, "kernel").expect("write kernel");
        fs::write(&rootfs, "rootfs").expect("write rootfs");

        ExecutionRuntimeConfig {
            backend: ExecutionBackendConfig::Firecracker(FirecrackerBackendConfig {
                mode: FirecrackerMode::Direct,
                firecracker_bin: firecracker_bin.display().to_string(),
                jailer_bin: "jailer".to_string(),
                kernel_image_path: kernel,
                rootfs_image_path: rootfs,
                guest_cid_base: 10_000,
                user_package_dirs: Vec::new(),
                networking: FirecrackerNetworkingConfig {
                    mode: FirecrackerNetworkingMode::Disabled,
                    privilege_mode: FirecrackerNetworkPrivilegeMode::Direct,
                    tap_name_prefix: "oomtap".to_string(),
                    proxy_port: 3128,
                    subnet_cidr: "172.22.0.0/16".to_string(),
                },
            }),
            runtime_dir: root.join("sandbox-runtime"),
            limits: ResourceLimits {
                vcpu_count: 1,
                memory_mib: 256,
                disk_quota_bytes: 256 * 1024 * 1024,
                timeout_secs: 5,
            },
            host_risk_posture: HostRiskPosture::IsolatedVm,
        }
    }

    #[test]
    fn build_backend_rejects_duplicate_guest_paths() {
        let temp = TempDir::new().expect("tempdir");
        let fake_firecracker = write_fake_firecracker(temp.path());
        let mut config = base_runtime_config(temp.path(), &fake_firecracker);
        config
            .firecracker_mut()
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
        let firecracker = config.firecracker_mut().expect("firecracker config");
        firecracker.mode = FirecrackerMode::Jailer;
        firecracker.jailer_bin = fake_firecracker.display().to_string();

        let backend = FirecrackerBackend::new(config).expect("backend");
        let err = backend
            .check_runtime_dependencies()
            .expect_err("jailer mode should fail");
        assert!(matches!(err, ExecutionError::NotImplemented(_)));
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
                .firecracker()
                .cloned()
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
                        bin: "codex".to_string(),
                        model: None,
                        auth_file: None,
                        api_key: None,
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
    fn stage_runtime_tree_copies_agent_auth_file_when_configured() {
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
                .firecracker()
                .cloned()
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
                        bin: "codex".to_string(),
                        model: None,
                        auth_file: Some(auth_file),
                        api_key: None,
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
                "cat /openoman-config/agent-auth.json",
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
                    bin: "codex".to_string(),
                    model: None,
                    auth_file: None,
                    api_key: None,
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
                .firecracker()
                .cloned()
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
                        bin: "codex".to_string(),
                        model: None,
                        auth_file: None,
                        api_key: None,
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
            .firecracker_mut()
            .expect("firecracker config")
            .networking
            .mode = FirecrackerNetworkingMode::HostProxy;
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker()
                .cloned()
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
                        bin: "codex".to_string(),
                        model: None,
                        auth_file: None,
                        api_key: None,
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
                .firecracker()
                .cloned()
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
    fn host_proxy_wildcard_allows_any_connect_domain() {
        let temp = TempDir::new().expect("tempdir");
        let fake_firecracker = write_fake_firecracker(temp.path());
        let config = base_runtime_config(temp.path(), &fake_firecracker);
        let runner = FirecrackerDirectRunner::new(
            config.runtime_dir.clone(),
            config
                .firecracker()
                .cloned()
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
            .start_network_proxy(&lease, &["*".to_string()], &log_path)
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

        assert!(
            response.starts_with("HTTP/1.1 200 Connection established")
                || response.starts_with("HTTP/1.1 502 Bad Gateway")
        );
        let log_contents = fs::read_to_string(&log_path).expect("read network log");
        assert!(!log_contents.contains("hostname not allowlisted"));
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
                    bin: fake_codex
                        .file_name()
                        .expect("codex filename")
                        .to_string_lossy()
                        .into_owned(),
                    model: None,
                    auth_file: None,
                    api_key: None,
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
                bin: "/usr/local/bin/codex".to_string(),
                model: None,
                auth_file: None,
                api_key: None,
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

        assert!(env_file.contains("export HTTPS_PROXY='http://proxy.internal:3128'"));
        assert!(env_file.contains("export HTTP_PROXY='http://proxy.internal:3128'"));
        assert!(env_file.contains("export https_proxy='http://proxy.internal:3128'"));
        assert!(env_file.contains("export http_proxy='http://proxy.internal:3128'"));
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
                bin: "/usr/local/bin/codex".to_string(),
                model: None,
                auth_file: None,
                api_key: None,
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
        assert!(env_file.contains("export HTTPS_PROXY='http://172.22.0.1:3128'"));
        assert!(env_file.contains("export HTTP_PROXY='http://172.22.0.1:3128'"));
        assert!(env_file.contains("export https_proxy='http://172.22.0.1:3128'"));
        assert!(env_file.contains("export http_proxy='http://172.22.0.1:3128'"));
    }

    #[test]
    fn render_agent_env_includes_host_proxy_debug_networking_markers() {
        let lease =
            allocate_network_lease("172.22.0.0/16", 1, "oomtap", 3128).expect("allocate lease");
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Cursor,
                bin: "/usr/local/bin/cursor-agent".to_string(),
                model: None,
                auth_file: None,
                api_key: Some("cursor-secret".to_string()),
                egress_proxy: None,
                egress_allowed_domains: vec!["*".to_string()],
            },
            &[],
            &[],
            Some(&lease),
        );

        assert!(env_file.contains("OPENOMAN_NET_ALLOW_ALL='1'"));
        assert!(env_file.contains("OPENOMAN_NET_HOST_IPV4='172.22.0.1'"));
    }

    #[test]
    fn render_agent_env_includes_agent_auth_file_when_configured() {
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Codex,
                bin: "/usr/local/bin/codex".to_string(),
                model: None,
                auth_file: Some(PathBuf::from("/host/.codex/auth.json")),
                api_key: None,
                egress_proxy: None,
                egress_allowed_domains: Vec::new(),
            },
            &[],
            &[],
            None,
        );

        assert!(env_file
            .contains("OPENOMAN_AGENT_AUTH_FILE='/mnt/runtime/openoman-config/agent-auth.json'"));
    }

    #[test]
    fn render_agent_env_includes_cursor_api_key() {
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Cursor,
                bin: "/usr/local/bin/cursor-agent".to_string(),
                model: None,
                auth_file: None,
                api_key: Some("cursor-secret".to_string()),
                egress_proxy: None,
                egress_allowed_domains: vec!["api2.cursor.sh".to_string()],
            },
            &[],
            &[],
            None,
        );

        assert!(env_file.contains("AGENT_PROVIDER=cursor"));
        assert!(env_file.contains("AGENT_BIN='/usr/local/bin/cursor-agent'"));
        assert!(env_file.contains("export CURSOR_API_KEY='cursor-secret'"));
    }

    #[test]
    fn render_agent_env_includes_cursor_model() {
        let env_file = render_agent_env(
            &AgentExecutionSpec {
                provider: AgentProvider::Cursor,
                bin: "/usr/local/bin/cursor-agent".to_string(),
                model: Some("gpt-5".to_string()),
                auth_file: None,
                api_key: Some("cursor-secret".to_string()),
                egress_proxy: None,
                egress_allowed_domains: vec!["api2.cursor.sh".to_string()],
            },
            &[],
            &[],
            None,
        );

        assert!(env_file.contains("AGENT_MODEL='gpt-5'"));
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

    fn real_firecracker_runtime_config(root: &Path, package_dir: &Path) -> ExecutionRuntimeConfig {
        ExecutionRuntimeConfig {
            backend: ExecutionBackendConfig::Firecracker(FirecrackerBackendConfig {
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
                networking: FirecrackerNetworkingConfig {
                    mode: FirecrackerNetworkingMode::Disabled,
                    privilege_mode: FirecrackerNetworkPrivilegeMode::Direct,
                    tap_name_prefix: "oomtap".to_string(),
                    proxy_port: 3128,
                    subnet_cidr: "172.22.0.0/16".to_string(),
                },
            }),
            runtime_dir: root.join("sandbox-runtime"),
            limits: ResourceLimits {
                vcpu_count: 1,
                memory_mib: 512,
                disk_quota_bytes: 512 * 1024 * 1024,
                timeout_secs: 30,
            },
            host_risk_posture: HostRiskPosture::IsolatedVm,
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
