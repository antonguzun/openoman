use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::agents::{
    auth_install_path, build_launch_plan, AgentLaunchContext, AgentLaunchPlan, AgentReportMode,
};

use super::{
    AttemptSpec, CollectedExecutionOutput, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionError, ExecutionExitStatus, ExecutionHandle, ExecutionIsolation,
    ExecutionRunner, ExecutionRuntimeConfig, HostRiskPosture,
};

#[derive(Debug)]
pub struct ProcessBackend {
    runtime: ExecutionRuntimeConfig,
}

impl ProcessBackend {
    pub fn new(runtime: ExecutionRuntimeConfig) -> Self {
        Self { runtime }
    }
}

impl ExecutionBackend for ProcessBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Process
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities {
            isolation: ExecutionIsolation::HostProcess,
            requires_explicit_risk_acknowledgement: true,
            supports_host_proxy_networking: false,
        }
    }

    fn check_runtime_dependencies(&self) -> Result<(), ExecutionError> {
        validate_runtime_dir(&self.runtime.runtime_dir)?;
        if self.runtime.host_risk_posture != HostRiskPosture::AlreadyIsolated {
            return Err(ExecutionError::InvalidConfig(
                "execution backend process requires host_risk_posture = \"already_isolated\""
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn create_runner(&self) -> Result<Box<dyn ExecutionRunner>, ExecutionError> {
        Ok(Box::new(ProcessRunner::new(
            self.runtime.runtime_dir.clone(),
        )))
    }
}

#[derive(Debug)]
struct RunningProcessAttempt {
    child: Child,
    timeout: Duration,
    run_dir: PathBuf,
}

#[derive(Debug)]
struct ProcessRunner {
    root_dir: PathBuf,
    next_handle_id: u64,
    running: HashMap<u64, RunningProcessAttempt>,
}

impl ProcessRunner {
    fn new(root_dir: PathBuf) -> Self {
        Self {
            root_dir,
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

    fn workspace_dir(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("workspace")
    }

    fn home_dir(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("home")
    }

    fn report_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("report.txt")
    }

    fn logs_path(handle: &ExecutionHandle) -> PathBuf {
        handle.run_dir.join("logs.txt")
    }

    fn stage_workspace(
        &self,
        source_workspace: &Path,
        destination_workspace: &Path,
    ) -> Result<(), ExecutionError> {
        if destination_workspace.exists() {
            fs::remove_dir_all(destination_workspace)?;
        }
        copy_tree(source_workspace, destination_workspace)
    }

    fn install_agent_auth_file(
        &self,
        spec: &AttemptSpec,
        plan: &AgentLaunchPlan,
        home_dir: &Path,
    ) -> Result<(), ExecutionError> {
        let Some(auth_file) = &spec.agent.auth_file else {
            return Ok(());
        };
        let Some(relative_target) = &plan.auth_file_home_relative_path else {
            return Ok(());
        };

        let destination = auth_install_path(home_dir, relative_target);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(auth_file, destination)?;
        Ok(())
    }

    fn append_log_line(log_path: &Path, message: &str) -> Result<(), ExecutionError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        writeln!(file, "{message}")?;
        Ok(())
    }

    fn write_log_preamble(
        &self,
        spec: &AttemptSpec,
        plan: &AgentLaunchPlan,
        log_path: &Path,
    ) -> Result<(), ExecutionError> {
        Self::append_log_line(log_path, "openoman process backend started")?;
        Self::append_log_line(log_path, &format!("agent provider: {}", plan.provider_id))?;
        Self::append_log_line(log_path, &format!("agent bin: {}", plan.binary))?;
        Self::append_log_line(log_path, &format!("instruction: {}", spec.instruction))?;
        if let Some(model) = &spec.agent.model {
            Self::append_log_line(log_path, &format!("agent model: {model}"))?;
        }
        if let Some(proxy) = &spec.agent.egress_proxy {
            Self::append_log_line(log_path, &format!("egress proxy configured: {proxy}"))?;
        }
        if !spec.agent.egress_allowed_domains.is_empty() {
            Self::append_log_line(
                log_path,
                &format!(
                    "egress allowed domains: {}",
                    spec.agent.egress_allowed_domains.join(",")
                ),
            )?;
        }
        Ok(())
    }

    fn spawn_agent_process(
        &self,
        spec: &AttemptSpec,
        handle: &ExecutionHandle,
    ) -> Result<Child, ExecutionError> {
        let home_dir = Self::home_dir(handle);
        let report_path = Self::report_path(handle);
        let logs_path = Self::logs_path(handle);
        let workspace_dir = Self::workspace_dir(handle);
        let report_path_string = report_path.display().to_string();
        let workspace_dir_string = workspace_dir.display().to_string();
        let plan = build_launch_plan(
            &spec.agent,
            AgentLaunchContext {
                binary: &spec.agent.bin,
                workspace_dir: &workspace_dir_string,
                report_path: &report_path_string,
                instruction: &spec.instruction,
            },
        )?;

        let executable = resolve_command_path(&plan.binary)?;
        self.install_agent_auth_file(spec, &plan, &home_dir)?;
        self.write_log_preamble(spec, &plan, &logs_path)?;

        let mut command = Command::new(executable);
        let path =
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
        command.env_clear();
        command.env("PATH", path);
        command.env("HOME", &home_dir);
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            command.env("TMPDIR", tmpdir);
        }
        if let Ok(lang) = std::env::var("LANG") {
            command.env("LANG", lang);
        }
        if let Ok(lc_all) = std::env::var("LC_ALL") {
            command.env("LC_ALL", lc_all);
        }
        if !spec.agent.egress_allowed_domains.is_empty() {
            command.env(
                "OPENOMAN_EGRESS_ALLOWED_DOMAINS",
                spec.agent.egress_allowed_domains.join(","),
            );
        }
        if let Some(proxy) = &spec.agent.egress_proxy {
            command.env("HTTPS_PROXY", proxy);
            command.env("HTTP_PROXY", proxy);
            command.env("https_proxy", proxy);
            command.env("http_proxy", proxy);
        }
        for (key, value) in &plan.env {
            command.env(key, value);
        }

        let logs_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&logs_path)?;
        command.current_dir(Path::new(&plan.working_directory));
        command.args(&plan.args);
        // Never inherit the caller's stdin: `claude -p` reads a non-TTY stdin to
        // EOF, so a held-open pipe would hang the attempt and any piped bytes
        // would be injected into the agent's prompt.
        command.stdin(Stdio::null());
        match plan.report_mode {
            AgentReportMode::File => {
                command.stdout(Stdio::from(logs_file.try_clone()?));
                command.stderr(Stdio::from(logs_file));
            }
            AgentReportMode::Stdout => {
                let report_file = fs::File::create(&report_path)?;
                command.stdout(Stdio::from(report_file));
                command.stderr(Stdio::from(logs_file));
            }
        }

        command.spawn().map_err(ExecutionError::Io)
    }

    fn collect_workspace_and_logs(
        &self,
        handle: &ExecutionHandle,
        output_dir: &Path,
    ) -> Result<CollectedExecutionOutput, ExecutionError> {
        if output_dir.exists() {
            fs::remove_dir_all(output_dir)?;
        }
        fs::create_dir_all(output_dir)?;

        let modified_workspace_dir = output_dir.join("workspace-result");
        copy_tree(&Self::workspace_dir(handle), &modified_workspace_dir)?;

        let report_path = output_dir.join("report.txt");
        copy_text_artifact_or_default(
            &Self::report_path(handle),
            &report_path,
            "Sandbox execution completed without report output.\n",
        )?;

        let logs_path = output_dir.join("logs.txt");
        copy_text_artifact_or_default(
            &Self::logs_path(handle),
            &logs_path,
            "sandbox execution did not produce logs\n",
        )?;

        Ok(CollectedExecutionOutput {
            modified_workspace_dir,
            report_path,
            logs_path,
        })
    }
}

impl ExecutionRunner for ProcessRunner {
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

        let workspace_dir = Self::workspace_dir(&handle);
        fs::create_dir_all(Self::home_dir(&handle))?;
        if let Err(err) = self.stage_workspace(&spec.workspace_dir, &workspace_dir) {
            let _ = fs::remove_dir_all(&run_dir);
            return Err(err);
        }

        let child = match self.spawn_agent_process(&spec, &handle) {
            Ok(child) => child,
            Err(err) => {
                let _ = fs::remove_dir_all(&run_dir);
                return Err(err);
            }
        };

        self.running.insert(
            handle_id,
            RunningProcessAttempt {
                child,
                timeout: Duration::from_secs(spec.limits.timeout_secs.max(1)),
                run_dir,
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
        loop {
            let status = {
                let running = self.running.get_mut(&handle.id).ok_or_else(|| {
                    ExecutionError::RunnerState(format!("unknown handle {}", handle.id))
                })?;
                running.child.try_wait()?
            };
            if let Some(status) = status {
                let _ = Self::append_log_line(
                    &Self::logs_path(handle),
                    &format!("process exited with status {:?}", status.code()),
                );
                return Ok(status.into());
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
                let _ = Self::append_log_line(
                    &Self::logs_path(handle),
                    "process timed out and was terminated",
                );
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
        self.collect_workspace_and_logs(handle, &self.stable_output_dir(job_id, attempt_id))
    }

    fn stop(&mut self, handle: &ExecutionHandle) -> Result<(), ExecutionError> {
        if let Some(mut running) = self.running.remove(&handle.id) {
            if running.child.try_wait()?.is_none() {
                running.child.kill()?;
                let _ = running.child.wait();
            }
            if running.run_dir.exists() {
                fs::remove_dir_all(&running.run_dir)?;
            }
        } else if handle.run_dir.exists() {
            fs::remove_dir_all(&handle.run_dir)?;
        }

        Ok(())
    }
}

fn validate_runtime_dir(path: &Path) -> Result<(), ExecutionError> {
    fs::create_dir_all(path)?;
    let probe = path.join(".openoman-write-probe");
    fs::write(&probe, b"ok")?;
    fs::remove_file(probe)?;
    Ok(())
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

fn copy_text_artifact_or_default(
    source: &Path,
    destination: &Path,
    default_contents: &str,
) -> Result<(), ExecutionError> {
    if source.exists() {
        fs::copy(source, destination)?;
    } else {
        fs::write(destination, default_contents)?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        copy_symlink(source, destination)?;
        return Ok(());
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        return Ok(());
    }

    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let child_source = entry.path();
        let child_destination = destination.join(entry.file_name());
        copy_tree(&child_source, &child_destination)?;
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let target = fs::read_link(source)?;
    symlink(target, destination)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
    let resolved = fs::canonicalize(source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(resolved, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{ExecutionBackendConfig, ResourceLimits};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn base_runtime_config(root: &Path) -> ExecutionRuntimeConfig {
        ExecutionRuntimeConfig {
            backend: ExecutionBackendConfig::Process,
            runtime_dir: root.join("process-runtime"),
            limits: ResourceLimits {
                vcpu_count: 1,
                memory_mib: 256,
                disk_quota_bytes: 256 * 1024 * 1024,
                timeout_secs: 5,
            },
            host_risk_posture: HostRiskPosture::AlreadyIsolated,
        }
    }

    #[test]
    fn process_backend_requires_explicit_risk_acknowledgement() {
        let temp = TempDir::new().expect("tempdir");
        let mut config = base_runtime_config(temp.path());
        config.host_risk_posture = HostRiskPosture::IsolatedVm;

        let backend = ProcessBackend::new(config);
        let err = backend
            .check_runtime_dependencies()
            .expect_err("risk posture should be required");
        assert!(err
            .to_string()
            .contains("host_risk_posture = \"already_isolated\""));
    }

    #[test]
    fn process_runner_collects_workspace_report_and_logs_for_codex() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let fake_codex = write_fake_codex(temp.path());
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write readme");

        let config = base_runtime_config(temp.path());
        let backend = ProcessBackend::new(config.clone());
        backend
            .check_runtime_dependencies()
            .expect("process dependencies available");

        let mut runner = backend.create_runner().expect("runner");
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-process-codex".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "append blank line to readme".to_string(),
                limits: config.limits.clone(),
                agent: super::super::AgentExecutionSpec {
                    provider: "codex".to_string(),
                    bin: fake_codex.display().to_string(),
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
            .collect_output(&handle, "job-process-codex", 1)
            .expect("collect output");
        assert_eq!(
            fs::read_to_string(output.modified_workspace_dir.join("README.md")).expect("readme"),
            "hello\n\n"
        );
        assert!(fs::read_to_string(&output.report_path)
            .expect("report")
            .contains("fake codex completed"));
        let logs = fs::read_to_string(&output.logs_path).expect("logs");
        assert!(logs.contains("openoman process backend started"));
        assert!(logs.contains("fake codex applied instruction"));

        runner.stop(&handle).expect("stop");
        assert!(!handle.run_dir.exists());
    }

    #[test]
    fn process_runner_collects_stdout_report_for_cursor() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let fake_cursor = write_fake_cursor(temp.path());
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(workspace.join("README.md"), "hello\n").expect("write readme");

        let config = base_runtime_config(temp.path());
        let backend = ProcessBackend::new(config.clone());
        let mut runner = backend.create_runner().expect("runner");
        let handle = runner
            .start(AttemptSpec {
                job_id: "job-process-cursor".to_string(),
                attempt_id: 1,
                workspace_dir: workspace,
                instruction: "append blank line to readme".to_string(),
                limits: config.limits.clone(),
                agent: super::super::AgentExecutionSpec {
                    provider: "cursor".to_string(),
                    bin: fake_cursor.display().to_string(),
                    model: Some("gpt-5".to_string()),
                    auth_file: None,
                    api_key: Some("cursor-test-key".to_string()),
                    egress_proxy: None,
                    egress_allowed_domains: vec!["api2.cursor.sh".to_string()],
                },
            })
            .expect("start");

        let status = runner.wait(&handle).expect("wait");
        assert!(status.success);

        let output = runner
            .collect_output(&handle, "job-process-cursor", 1)
            .expect("collect output");
        assert_eq!(
            fs::read_to_string(output.modified_workspace_dir.join("README.md")).expect("readme"),
            "hello\n\n"
        );
        let report = fs::read_to_string(&output.report_path).expect("report");
        assert!(report.contains("fake cursor completed"));
        assert!(report.contains("model=gpt-5"));
        let logs = fs::read_to_string(&output.logs_path).expect("logs");
        assert!(logs.contains("agent provider: cursor"));

        runner.stop(&handle).expect("stop");
    }

    fn write_fake_codex(root: &Path) -> PathBuf {
        let script_path = root.join("fake-codex.sh");
        fs::write(
            &script_path,
            r#"#!/usr/bin/env sh
set -eu
if [ "${1:-}" = "exec" ]; then
  shift
else
  echo "unexpected invocation" >&2
  exit 2
fi
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
    --dangerously-bypass-approvals-and-sandbox)
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
printf "fake codex completed: %s\n" "$instruction" > "$report"
echo "fake codex applied instruction"
"#,
        )
        .expect("write fake codex");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .expect("chmod fake codex");
        script_path
    }

    fn write_fake_cursor(root: &Path) -> PathBuf {
        let script_path = root.join("fake-cursor-agent.sh");
        fs::write(
            &script_path,
            r#"#!/usr/bin/env sh
set -eu
if [ "${CURSOR_API_KEY:-}" = "" ]; then
  echo "missing CURSOR_API_KEY" >&2
  exit 11
fi
print_mode=0
model=""
explicit_api_key=""
instruction=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --api-key)
      explicit_api_key="$2"
      shift 2
      ;;
    -p|-f)
      print_mode=1
      shift 1
      ;;
    --output-format)
      shift 2
      ;;
    --model)
      model="$2"
      shift 2
      ;;
    *)
      instruction="$1"
      shift 1
      ;;
  esac
done
if [ "$print_mode" -ne 1 ]; then
  echo "expected print mode" >&2
  exit 12
fi
if [ "$explicit_api_key" != "${CURSOR_API_KEY:-}" ]; then
  echo "--api-key mismatch" >&2
  exit 15
fi
printf "\n" >> README.md
echo "fake cursor completed: ${instruction}${model:+ (model=$model)}"
"#,
        )
        .expect("write fake cursor");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .expect("chmod fake cursor");
        script_path
    }
}
