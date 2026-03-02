use std::path::PathBuf;

use super::{firecracker::FirecrackerBackend, ResourceLimits, SandboxError, SandboxRunner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackendKind {
    Firecracker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirecrackerMode {
    Direct,
    Jailer,
}

#[derive(Debug, Clone)]
pub struct UserPackageDir {
    pub host_path: PathBuf,
    pub guest_path: PathBuf,
    pub add_to_path: bool,
}

#[derive(Debug, Clone)]
pub struct FirecrackerBackendConfig {
    pub mode: FirecrackerMode,
    pub firecracker_bin: String,
    pub jailer_bin: String,
    pub kernel_image_path: PathBuf,
    pub rootfs_image_path: PathBuf,
    pub guest_cid_base: u32,
    pub user_package_dirs: Vec<UserPackageDir>,
}

#[derive(Debug, Clone)]
pub struct SandboxRuntimeConfig {
    pub backend: SandboxBackendKind,
    pub runtime_dir: PathBuf,
    pub limits: ResourceLimits,
    pub firecracker: Option<FirecrackerBackendConfig>,
}

pub trait SandboxBackend {
    fn kind(&self) -> SandboxBackendKind;
    fn check_runtime_dependencies(&self) -> Result<(), SandboxError>;
    fn create_runner(&self) -> Result<Box<dyn SandboxRunner>, SandboxError>;
}

pub fn build_sandbox_backend(
    config: SandboxRuntimeConfig,
) -> Result<Box<dyn SandboxBackend>, SandboxError> {
    match config.backend {
        SandboxBackendKind::Firecracker => Ok(Box::new(FirecrackerBackend::new(config)?)),
    }
}
