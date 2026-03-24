use std::{path::PathBuf, process::ExitStatus};

mod backend;
mod firecracker;
mod process;

pub use backend::{
    build_execution_backend, DockerAuthConfig, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendConfig, ExecutionBackendKind, ExecutionIsolation, ExecutionRuntimeConfig,
    FirecrackerBackendConfig, FirecrackerMode, FirecrackerNetworkPrivilegeMode,
    FirecrackerNetworkingConfig, FirecrackerNetworkingMode, HostRiskPosture, UserPackageDir,
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

#[derive(Clone)]
pub struct AgentExecutionSpec {
    pub provider: String,
    pub bin: String,
    pub model: Option<String>,
    pub auth_file: Option<PathBuf>,
    pub api_key: Option<String>,
    pub egress_proxy: Option<String>,
    pub egress_allowed_domains: Vec<String>,
}

impl std::fmt::Debug for AgentExecutionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentExecutionSpec")
            .field("provider", &self.provider)
            .field("bin", &self.bin)
            .field("model", &self.model)
            .field("auth_file", &self.auth_file)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("egress_proxy", &self.egress_proxy)
            .field("egress_allowed_domains", &self.egress_allowed_domains)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionHandle {
    pub id: u64,
    pub run_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ExecutionExitStatus {
    pub success: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
}

impl From<ExitStatus> for ExecutionExitStatus {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
            timed_out: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CollectedExecutionOutput {
    pub modified_workspace_dir: PathBuf,
    pub report_path: PathBuf,
    pub logs_path: PathBuf,
}

pub trait ExecutionRunner {
    fn start(&mut self, spec: AttemptSpec) -> Result<ExecutionHandle, ExecutionError>;
    fn wait(&mut self, handle: &ExecutionHandle) -> Result<ExecutionExitStatus, ExecutionError>;
    fn collect_output(
        &self,
        handle: &ExecutionHandle,
        job_id: &str,
        attempt_id: u32,
    ) -> Result<CollectedExecutionOutput, ExecutionError>;
    fn stop(&mut self, handle: &ExecutionHandle) -> Result<(), ExecutionError>;
}

#[derive(Debug)]
pub enum ExecutionError {
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

impl std::fmt::Display for ExecutionError {
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

impl std::error::Error for ExecutionError {}

impl From<std::io::Error> for ExecutionError {
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
