use std::path::PathBuf;

use super::{
    firecracker::FirecrackerBackend, process::ProcessBackend, ExecutionError, ExecutionRunner,
    ResourceLimits,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionBackendKind {
    Firecracker,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRiskPosture {
    IsolatedVm,
    AlreadyIsolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionIsolation {
    MicroVm,
    HostProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionBackendCapabilities {
    pub isolation: ExecutionIsolation,
    pub requires_explicit_risk_acknowledgement: bool,
    pub supports_host_proxy_networking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirecrackerMode {
    Direct,
    Jailer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirecrackerNetworkingMode {
    Disabled,
    HostProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirecrackerNetworkPrivilegeMode {
    Sudo,
    Direct,
}

#[derive(Debug, Clone)]
pub struct FirecrackerNetworkingConfig {
    pub mode: FirecrackerNetworkingMode,
    pub privilege_mode: FirecrackerNetworkPrivilegeMode,
    pub tap_name_prefix: String,
    pub proxy_port: u16,
    pub allowed_connect_ports: Vec<u16>,
    pub subnet_cidr: String,
}

#[derive(Debug, Clone)]
pub struct UserPackageDir {
    pub host_path: PathBuf,
    pub guest_path: PathBuf,
    pub add_to_path: bool,
}

#[derive(Debug, Clone)]
pub enum DockerAuthConfig {
    HostFile(PathBuf),
    InlineJson(String),
}

#[derive(Debug, Clone)]
pub struct FirecrackerBackendConfig {
    pub mode: FirecrackerMode,
    pub firecracker_bin: String,
    pub jailer_bin: String,
    pub kernel_image_path: PathBuf,
    pub rootfs_image_path: PathBuf,
    pub runtime_disk_bytes: Option<u64>,
    pub guest_cid_base: u32,
    pub docker_daemon: bool,
    pub docker_auth_config: Option<DockerAuthConfig>,
    pub user_package_dirs: Vec<UserPackageDir>,
    pub networking: FirecrackerNetworkingConfig,
}

#[derive(Debug, Clone)]
pub enum ExecutionBackendConfig {
    Firecracker(FirecrackerBackendConfig),
    Process,
}

impl ExecutionBackendConfig {
    pub fn kind(&self) -> ExecutionBackendKind {
        match self {
            Self::Firecracker(_) => ExecutionBackendKind::Firecracker,
            Self::Process => ExecutionBackendKind::Process,
        }
    }

    pub fn firecracker(&self) -> Option<&FirecrackerBackendConfig> {
        match self {
            Self::Firecracker(config) => Some(config),
            Self::Process => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionRuntimeConfig {
    pub backend: ExecutionBackendConfig,
    pub runtime_dir: PathBuf,
    pub limits: ResourceLimits,
    pub host_risk_posture: HostRiskPosture,
}

impl ExecutionRuntimeConfig {
    pub fn backend_kind(&self) -> ExecutionBackendKind {
        self.backend.kind()
    }

    pub fn firecracker(&self) -> Option<&FirecrackerBackendConfig> {
        self.backend.firecracker()
    }

    pub fn firecracker_mut(&mut self) -> Option<&mut FirecrackerBackendConfig> {
        match &mut self.backend {
            ExecutionBackendConfig::Firecracker(config) => Some(config),
            ExecutionBackendConfig::Process => None,
        }
    }
}

pub trait ExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind;
    fn capabilities(&self) -> ExecutionBackendCapabilities;
    fn check_runtime_dependencies(&self) -> Result<(), ExecutionError>;
    fn create_runner(&self) -> Result<Box<dyn ExecutionRunner>, ExecutionError>;
}

pub fn build_execution_backend(
    config: ExecutionRuntimeConfig,
) -> Result<Box<dyn ExecutionBackend>, ExecutionError> {
    match config.backend_kind() {
        ExecutionBackendKind::Firecracker => Ok(Box::new(FirecrackerBackend::new(config)?)),
        ExecutionBackendKind::Process => Ok(Box::new(ProcessBackend::new(config))),
    }
}
