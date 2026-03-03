use std::{path::PathBuf, process::ExitStatus};

mod backend;
mod firecracker;

pub use backend::{
    build_sandbox_backend, FirecrackerBackendConfig, FirecrackerMode,
    FirecrackerNetworkPrivilegeMode, FirecrackerNetworkingConfig, FirecrackerNetworkingMode,
    SandboxBackend, SandboxBackendKind, SandboxRuntimeConfig, UserPackageDir,
};

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub vcpu_count: u8,
    pub memory_mib: u32,
    pub disk_quota_bytes: u64,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct AttemptSpec {
    pub job_id: String,
    pub attempt_id: u32,
    pub workspace_dir: PathBuf,
    pub instruction: String,
    pub limits: ResourceLimits,
    pub agent: AgentExecutionSpec,
}

#[derive(Debug, Clone)]
pub struct AgentExecutionSpec {
    pub provider: AgentProvider,
    pub codex_bin: String,
    pub codex_auth_file: Option<PathBuf>,
    pub egress_proxy: Option<String>,
    pub egress_allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProvider {
    Codex,
}

#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub id: u64,
    pub run_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SandboxExitStatus {
    pub success: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
}

impl From<ExitStatus> for SandboxExitStatus {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
            timed_out: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CollectedSandboxOutput {
    pub modified_workspace_dir: PathBuf,
    pub report_path: PathBuf,
    pub logs_path: PathBuf,
}

pub trait SandboxRunner {
    fn start(&mut self, spec: AttemptSpec) -> Result<SandboxHandle, SandboxError>;
    fn wait(&mut self, handle: &SandboxHandle) -> Result<SandboxExitStatus, SandboxError>;
    fn collect_output(
        &self,
        handle: &SandboxHandle,
        job_id: &str,
        attempt_id: u32,
    ) -> Result<CollectedSandboxOutput, SandboxError>;
    fn stop(&mut self, handle: &SandboxHandle) -> Result<(), SandboxError>;
}

#[derive(Debug)]
pub enum SandboxError {
    Io(std::io::Error),
    RunnerState(String),
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    MissingDependency(String),
    InvalidConfig(String),
    NotImplemented(String),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::RunnerState(msg) => write!(f, "runner state error: {msg}"),
            Self::CommandFailed {
                program,
                args,
                stderr,
            } => write!(
                f,
                "command failed: {} {}\n{}",
                program,
                args.join(" "),
                stderr
            ),
            Self::MissingDependency(msg) => write!(f, "missing dependency: {msg}"),
            Self::InvalidConfig(msg) => write!(f, "invalid config: {msg}"),
            Self::NotImplemented(msg) => write!(f, "not implemented: {msg}"),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<std::io::Error> for SandboxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }

    format!("'{}'", input.replace('\'', "'\"'\"'"))
}
