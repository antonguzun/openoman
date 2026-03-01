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
    pub agent: AgentExecutionSpec,
}

#[derive(Debug, Clone)]
pub struct AgentExecutionSpec {
    pub provider: AgentProvider,
    pub codex_bin: String,
    pub egress_proxy: Option<String>,
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

    fn stable_output_dir(&self, job_id: &str, attempt_id: u32) -> PathBuf {
        self.root_dir
            .join("jobs")
            .join(job_id)
            .join(format!("attempt-{attempt_id}"))
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
        fs::create_dir_all(&guest_workspace)?;
        fs::create_dir_all(&guest_artifacts)?;

        self.copy_tree_with_tar(&spec.workspace_dir, &guest_workspace)?;

        let script_path = guest_dir.join("run-smoke.sh");
        let mut script = fs::File::create(&script_path)?;
        let quoted_instruction = shell_quote(&spec.instruction);
        let quoted_codex_bin = shell_quote(&spec.agent.codex_bin);

        writeln!(script, "#!/usr/bin/env sh")?;
        writeln!(script, "set -u")?;
        writeln!(script, "agent_status=0")?;
        writeln!(script, "echo 'sandbox started' > artifacts/logs.txt")?;
        writeln!(
            script,
            "echo 'instruction: {}' >> artifacts/logs.txt",
            spec.instruction.replace('\'', "")
        )?;
        if let Some(proxy) = &spec.agent.egress_proxy {
            let sanitized = proxy.replace('\'', "");
            writeln!(script, "export HTTPS_PROXY='{}'", sanitized)?;
            writeln!(script, "export HTTP_PROXY='{}'", sanitized)?;
            writeln!(
                script,
                "echo 'egress proxy configured: {}' >> artifacts/logs.txt",
                sanitized
            )?;
        }
        match spec.agent.provider {
            AgentProvider::Codex => {
                writeln!(
                    script,
                    "if command -v {} >/dev/null 2>&1; then",
                    quoted_codex_bin
                )?;
                writeln!(
                    script,
                    "  {} --version >> artifacts/logs.txt 2>&1 || true",
                    quoted_codex_bin
                )?;
                writeln!(
                    script,
                    "  {} exec --full-auto --color never -C workspace -o artifacts/report.txt {} >> artifacts/logs.txt 2>&1",
                    quoted_codex_bin,
                    quoted_instruction
                )?;
                writeln!(script, "  agent_status=$?")?;
                writeln!(script, "  if [ \"$agent_status\" -ne 0 ]; then")?;
                writeln!(
                    script,
                    "    echo \"codex execution failed with exit code ${{agent_status}}\" >> artifacts/logs.txt"
                )?;
                writeln!(script, "  fi")?;
                writeln!(script, "else")?;
                writeln!(script, "  agent_status=127")?;
                writeln!(
                    script,
                    "  echo 'codex binary not found: {}' >> artifacts/logs.txt",
                    spec.agent.codex_bin.replace('\'', "")
                )?;
                writeln!(script, "fi")?;
            }
        }

        writeln!(script, "if [ ! -f artifacts/report.txt ]; then")?;
        writeln!(
            script,
            "  echo 'Sandbox execution completed without report output.' > artifacts/report.txt"
        )?;
        writeln!(script, "fi")?;
        writeln!(script, "exit \"${{agent_status}}\"")?;

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

        let guest_workspace = handle.run_dir.join("guest").join("workspace");
        let guest_artifacts = handle.run_dir.join("guest").join("artifacts");
        let modified_workspace_dir = output_dir.join("workspace-result");
        self.copy_tree_with_tar(&guest_workspace, &modified_workspace_dir)?;
        self.copy_tree_with_tar(&guest_artifacts, &output_dir)?;

        Ok(CollectedSandboxOutput {
            modified_workspace_dir,
            report_path: output_dir.join("report.txt"),
            logs_path: output_dir.join("logs.txt"),
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

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }

    format!("'{}'", input.replace('\'', "'\"'\"'"))
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
    fn lifecycle_collects_workspace_report_and_logs() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let fake_codex = write_fake_codex(temp.path());
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write workspace file");

        let mut runner = FirecrackerRunner::new(temp.path().join("sandboxes"));
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-1".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "append blank line to readme".to_string(),
                limits: limits(5),
                agent: AgentExecutionSpec {
                    provider: AgentProvider::Codex,
                    codex_bin: fake_codex.display().to_string(),
                    egress_proxy: Some("http://proxy.internal:3128".to_string()),
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

        let report = fs::read_to_string(&output.report_path).expect("report");
        assert!(report.contains("fake codex completed"));
        let logs = fs::read_to_string(&output.logs_path).expect("logs");
        assert!(logs.contains("egress proxy configured: http://proxy.internal:3128"));
        assert!(logs.contains("codex-cli 0.0-test"));
        assert_eq!(
            fs::read_to_string(output.modified_workspace_dir.join("README.md")).expect("readme"),
            "hello\n\n"
        );

        runner.stop(&handle).expect("stop");
        assert!(!handle.run_dir.exists());
        assert!(output.modified_workspace_dir.exists());
    }

    #[test]
    fn wait_marks_timeout_when_process_exceeds_limit() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let fake_codex = write_fake_codex(temp.path());
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
                agent: AgentExecutionSpec {
                    provider: AgentProvider::Codex,
                    codex_bin: fake_codex.display().to_string(),
                    egress_proxy: None,
                },
            })
            .expect("start");

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

    fn write_fake_codex(root: &Path) -> PathBuf {
        let script_path = root.join("fake-codex.sh");
        fs::write(
            &script_path,
            r#"#!/usr/bin/env sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "codex-cli 0.0-test"
  exit 0
fi
if [ "${1:-}" != "exec" ]; then
  echo "unexpected invocation" >&2
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
    --full-auto|--color)
      if [ "$1" = "--color" ]; then
        shift 2
      else
        shift 1
      fi
      ;;
    *)
      instruction="$1"
      shift 1
      ;;
  esac
done
if [ -z "$report" ]; then
  echo "missing report path" >&2
  exit 3
fi
cd "$workdir"
if printf "%s" "$instruction" | grep -q fail; then
  printf "\n" >> README.md
  printf "fake codex failed\n" > "../$report"
  echo "simulated failure"
  exit 9
fi
printf "\n" >> README.md
printf "fake codex completed: %s\n" "$instruction" > "../$report"
echo "fake codex applied instruction"
"#,
        )
        .expect("write fake codex");
        fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod fake codex");
        script_path
    }
}
