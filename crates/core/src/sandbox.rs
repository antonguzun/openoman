use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
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
}

#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub id: u64,
    pub run_dir: PathBuf,
    pub host_workspace_copy: PathBuf,
    pub host_artifacts_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SandboxExitStatus {
    pub success: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct CollectedArtifacts {
    pub patch_path: PathBuf,
    pub report_path: PathBuf,
    pub logs_path: PathBuf,
}

pub trait SandboxRunner {
    fn start(&mut self, spec: AttemptSpec) -> Result<SandboxHandle, SandboxError>;
    fn wait(&mut self, handle: &SandboxHandle) -> Result<SandboxExitStatus, SandboxError>;
    fn collect_artifacts(&self, handle: &SandboxHandle)
        -> Result<CollectedArtifacts, SandboxError>;
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
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<std::io::Error> for SandboxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
struct RunningSandbox {
    child: Child,
    timeout: Duration,
}

#[derive(Debug)]
pub struct FirecrackerRunner {
    root_dir: PathBuf,
    next_handle_id: u64,
    running: HashMap<u64, RunningSandbox>,
}

impl FirecrackerRunner {
    pub fn new(root_dir: impl AsRef<Path>) -> Self {
        Self {
            root_dir: root_dir.as_ref().to_path_buf(),
            next_handle_id: 1,
            running: HashMap::new(),
        }
    }

    fn run_tar(&self, cwd: &Path, args: &[&str]) -> Result<(), SandboxError> {
        let output = Command::new("tar").current_dir(cwd).args(args).output()?;
        if output.status.success() {
            return Ok(());
        }

        Err(SandboxError::CommandFailed {
            program: "tar".to_string(),
            args: args.iter().map(|x| x.to_string()).collect(),
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
        self.run_tar(
            source,
            &["-cf", archive.to_str().unwrap_or("tmp-copy.tar"), "."],
        )?;
        self.run_tar(
            destination,
            &["-xf", archive.to_str().unwrap_or("tmp-copy.tar")],
        )?;
        if archive.exists() {
            fs::remove_file(archive)?;
        }
        Ok(())
    }
}

impl SandboxRunner for FirecrackerRunner {
    fn start(&mut self, spec: AttemptSpec) -> Result<SandboxHandle, SandboxError> {
        let handle_id = self.next_handle_id;
        self.next_handle_id += 1;

        let run_dir = self.root_dir.join(format!(
            "{}-attempt-{}-{}",
            spec.job_id, spec.attempt_id, handle_id
        ));
        if run_dir.exists() {
            fs::remove_dir_all(&run_dir)?;
        }
        fs::create_dir_all(&run_dir)?;

        let guest_dir = run_dir.join("guest");
        let guest_workspace = guest_dir.join("workspace");
        let guest_artifacts = guest_dir.join("artifacts");
        let host_artifacts = self
            .root_dir
            .join("collected-artifacts")
            .join(format!("handle-{}", handle_id));
        fs::create_dir_all(&guest_workspace)?;
        fs::create_dir_all(&guest_artifacts)?;
        fs::create_dir_all(&host_artifacts)?;

        self.copy_tree_with_tar(&spec.workspace_dir, &guest_workspace)?;

        let script_path = guest_dir.join("run-smoke.sh");
        let mut script = fs::File::create(&script_path)?;
        writeln!(script, "#!/usr/bin/env sh")?;
        writeln!(script, "set -eu")?;
        writeln!(script, "echo 'sandbox started' > artifacts/logs.txt")?;
        writeln!(
            script,
            "echo 'instruction: {}' >> artifacts/logs.txt",
            spec.instruction.replace('\'', "")
        )?;
        writeln!(
            script,
            "echo 'Epic 5 smoke run complete.' > artifacts/report.txt"
        )?;
        writeln!(script, "echo '--- /dev/null' > artifacts/patch.diff")?;
        writeln!(script, "echo '+++ EPIC5_SMOKE.txt' >> artifacts/patch.diff")?;
        writeln!(script, "echo '@@ -0,0 +1 @@' >> artifacts/patch.diff")?;
        writeln!(
            script,
            "echo '+epic5 smoke output for {}' >> artifacts/patch.diff",
            spec.job_id
        )?;
        writeln!(
            script,
            "echo 'epic5 smoke output for {}' > workspace/EPIC5_SMOKE.txt",
            spec.job_id
        )?;
        fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;

        let child = Command::new("sh")
            .arg(
                script_path
                    .file_name()
                    .unwrap_or_else(|| OsStr::new("run-smoke.sh")),
            )
            .current_dir(&guest_dir)
            .spawn()?;

        self.running.insert(
            handle_id,
            RunningSandbox {
                child,
                timeout: Duration::from_secs(spec.limits.timeout_secs.max(1)),
            },
        );

        Ok(SandboxHandle {
            id: handle_id,
            run_dir,
            host_workspace_copy: guest_workspace,
            host_artifacts_dir: host_artifacts,
        })
    }

    fn wait(&mut self, handle: &SandboxHandle) -> Result<SandboxExitStatus, SandboxError> {
        let running = self
            .running
            .get_mut(&handle.id)
            .ok_or_else(|| SandboxError::RunnerState(format!("unknown handle {}", handle.id)))?;
        let start = Instant::now();

        loop {
            if let Some(status) = running.child.try_wait()? {
                return Ok(SandboxExitStatus {
                    success: status.success(),
                    code: status.code(),
                    timed_out: false,
                });
            }

            if start.elapsed() >= running.timeout {
                running.child.kill()?;
                let status = running.child.wait()?;
                return Ok(SandboxExitStatus {
                    success: false,
                    code: status.code(),
                    timed_out: true,
                });
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    fn collect_artifacts(
        &self,
        handle: &SandboxHandle,
    ) -> Result<CollectedArtifacts, SandboxError> {
        let guest_artifacts = handle.run_dir.join("guest").join("artifacts");
        self.copy_tree_with_tar(&guest_artifacts, &handle.host_artifacts_dir)?;

        Ok(CollectedArtifacts {
            patch_path: handle.host_artifacts_dir.join("patch.diff"),
            report_path: handle.host_artifacts_dir.join("report.txt"),
            logs_path: handle.host_artifacts_dir.join("logs.txt"),
        })
    }

    fn stop(&mut self, handle: &SandboxHandle) -> Result<(), SandboxError> {
        if let Some(mut running) = self.running.remove(&handle.id) {
            if running.child.try_wait()?.is_none() {
                running.child.kill()?;
                let _ = running.child.wait();
            }
        }

        if handle.run_dir.exists() {
            fs::remove_dir_all(&handle.run_dir)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn limits(timeout_secs: u64) -> ResourceLimits {
        ResourceLimits {
            vcpu_count: 1,
            memory_mib: 256,
            disk_quota_bytes: 100 * 1024 * 1024,
            timeout_secs,
        }
    }

    #[test]
    fn lifecycle_creates_and_collects_smoke_artifacts() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");

        let mut runner = FirecrackerRunner::new(temp.path().join("sandboxes"));
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-1".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "smoke".to_string(),
                limits: limits(5),
            })
            .expect("start");

        let status = runner.wait(&handle).expect("wait");
        assert!(status.success);
        let artifacts = runner.collect_artifacts(&handle).expect("collect");

        assert!(artifacts.patch_path.exists());
        assert!(artifacts.report_path.exists());
        assert!(artifacts.logs_path.exists());

        let report = fs::read_to_string(&artifacts.report_path).expect("report");
        assert!(report.contains("Epic 5 smoke run complete."));

        runner.stop(&handle).expect("stop");
        assert!(!handle.run_dir.exists());
    }

    #[test]
    fn wait_marks_timeout_when_process_exceeds_limit() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");

        let mut runner = FirecrackerRunner::new(temp.path().join("sandboxes"));
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-2".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "smoke".to_string(),
                limits: limits(1),
            })
            .expect("start");

        // Replace running process with a long sleep by starting new shell in-place for timeout verification.
        if let Some(state) = runner.running.get_mut(&handle.id) {
            let _ = state.child.kill();
            let _ = state.child.wait();
            state.child = Command::new("sh")
                .arg("-c")
                .arg("sleep 5")
                .spawn()
                .expect("spawn sleep");
            state.timeout = Duration::from_millis(150);
        }

        let status = runner.wait(&handle).expect("wait");
        assert!(status.timed_out);
        assert!(!status.success);
        runner.stop(&handle).expect("stop");
    }
}
