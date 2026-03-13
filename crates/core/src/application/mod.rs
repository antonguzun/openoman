use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    domain::{
        job::{ArtifactRef, Job, JobId, JobState, PublishResult},
        request::PublishPolicy,
    },
    execution::{
        AgentExecutionSpec, AttemptSpec, CollectedExecutionOutput, ExecutionBackend, ResourceLimits,
    },
    git::{write_canonical_patch, GitAdapter, PreparedWorkspace},
    github::{GitHubPublisher, GitHubPublisherConfig},
    gitlab::{GitLabPublisher, GitLabPublisherConfig},
    persistence::{NewArtifactRecord, NewOutboxEvent, OutboxStatus, SqliteStore},
};

pub struct RunJobUseCase {
    store: SqliteStore,
    git: GitAdapter,
    execution_backend: Box<dyn ExecutionBackend>,
    limits: ResourceLimits,
    agent_execution: AgentExecutionSpec,
    repo_env_source_dir: Option<PathBuf>,
    repo_clone_token: Option<String>,
    artifact_limits: RunJobArtifactLimits,
}

#[derive(Debug, Clone, Copy)]
pub struct RunJobArtifactLimits {
    pub log_limit_bytes: usize,
    pub report_limit_bytes: usize,
    pub patch_limit_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct RunJobOutcome {
    pub job_id: String,
    pub state: JobState,
    pub failure_reason: Option<String>,
    pub logs_path: Option<PathBuf>,
    pub publish_warning: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RunJobError {
    NotFound(String),
    InvalidState { job_id: String, state: JobState },
    Message(String),
}

#[derive(Debug, Clone)]
pub enum PublishExecutionPlan {
    GitHub(GitHubPublisherConfig),
    GitLab(GitLabPublisherConfig),
    SkipWithWarning(String),
}

impl RunJobUseCase {
    pub fn new(
        store: SqliteStore,
        git: GitAdapter,
        execution_backend: Box<dyn ExecutionBackend>,
        limits: ResourceLimits,
        agent_execution: AgentExecutionSpec,
        repo_env_source_dir: Option<PathBuf>,
        repo_clone_token: Option<String>,
        artifact_limits: RunJobArtifactLimits,
    ) -> Self {
        Self {
            store,
            git,
            execution_backend,
            limits,
            agent_execution,
            repo_env_source_dir,
            repo_clone_token,
            artifact_limits,
        }
    }

    pub fn run<F>(
        &self,
        job_id: &JobId,
        resolve_publisher_config: F,
    ) -> Result<RunJobOutcome, RunJobError>
    where
        F: FnOnce(&Job) -> Result<PublishExecutionPlan, RunJobError>,
    {
        let jobs = self.store.jobs();
        let Some(mut job) = jobs
            .load(job_id)
            .map_err(|e| RunJobError::Message(e.to_string()))?
        else {
            return Err(RunJobError::NotFound(job_id.as_str().to_string()));
        };

        if job.state != JobState::Queued {
            return Err(RunJobError::InvalidState {
                job_id: job.id.as_str().to_string(),
                state: job.state.clone(),
            });
        }

        let prepared = self
            .git
            .prepare_workspace_with_env_overlay_and_clone_token(
                &job.repo_ref,
                &job.revision,
                job.id.as_str(),
                self.repo_env_source_dir.as_deref(),
                self.repo_clone_token.as_deref(),
            )
            .map_err(|e| {
                RunJobError::Message(format!(
                    "failed to prepare git workspace for {} at revision {}: {e}",
                    job.repo_ref.as_str(),
                    job.revision.as_str()
                ))
            })?;
        let mut all_artifact_records = build_workspace_artifact_records(job.id.as_str(), &prepared)
            .map_err(RunJobError::Message)?;

        let attempt_id = 1;
        let mut runner = self
            .execution_backend
            .create_runner()
            .map_err(|e| RunJobError::Message(format!("failed to create sandbox runner: {e}")))?;
        let attempt_spec = AttemptSpec {
            job_id: job.id.as_str().to_string(),
            attempt_id,
            workspace_dir: prepared.sandbox_workspace_dir.clone(),
            instruction: job.instruction.clone(),
            limits: self.limits.clone(),
            agent: self.agent_execution.clone(),
        };

        let handle = runner
            .start(attempt_spec)
            .map_err(|e| RunJobError::Message(format!("failed to start sandbox attempt: {e}")))?;

        job.start_attempt(attempt_id)
            .map_err(|e| RunJobError::Message(e.to_string()))?;
        jobs.update(&job)
            .map_err(|e| RunJobError::Message(e.to_string()))?;

        let wait_result = runner
            .wait(&handle)
            .map_err(|e| format!("failed while waiting for sandbox attempt: {e}"));
        let collected = runner
            .collect_output(&handle, job.id.as_str(), attempt_id)
            .map_err(|e| format!("failed to collect sandbox output: {e}"));
        let stop_result = runner
            .stop(&handle)
            .map_err(|e| format!("failed to stop sandbox attempt: {e}"));

        let mut failure_reason = match &wait_result {
            Err(err) => Some(err.clone()),
            Ok(status) if status.timed_out => Some(format!(
                "sandbox attempt timed out for job {}",
                job.id.as_str()
            )),
            Ok(status) if !status.success => Some(format!(
                "sandbox attempt failed for job {} with exit code {:?}",
                job.id.as_str(),
                status.code
            )),
            Ok(_) => None,
        };

        let logs_path = collected
            .as_ref()
            .ok()
            .map(|output| output.logs_path.clone());

        if let Ok(collected) = &collected {
            let patch_path = collected
                .report_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("patch.diff");

            match build_workspace_result_artifact_records(job.id.as_str(), collected) {
                Ok(records) => all_artifact_records.extend(records),
                Err(err) if failure_reason.is_none() => {
                    failure_reason = Some(format!(
                        "failed to fingerprint sandbox workspace result: {err}"
                    ));
                }
                Err(_) => {}
            }

            if let Err(err) =
                prepare_text_artifact(&collected.logs_path, self.artifact_limits.log_limit_bytes)
            {
                if failure_reason.is_none() {
                    failure_reason = Some(format!("failed to prepare sandbox logs: {err}"));
                }
            }
            if let Err(err) = prepare_text_artifact(
                &collected.report_path,
                self.artifact_limits.report_limit_bytes,
            ) {
                if failure_reason.is_none() {
                    failure_reason = Some(format!("failed to prepare sandbox report: {err}"));
                }
            }

            match build_file_artifact_records(
                job.id.as_str(),
                &[
                    ("sandbox.report", collected.report_path.as_path()),
                    ("sandbox.logs", collected.logs_path.as_path()),
                ],
            ) {
                Ok(records) => all_artifact_records.extend(records),
                Err(err) if failure_reason.is_none() => {
                    failure_reason = Some(format!("failed to store sandbox text artifacts: {err}"));
                }
                Err(_) => {}
            }

            let patch_result = write_canonical_patch(
                &prepared.trusted_clone_dir,
                &collected.modified_workspace_dir,
                &patch_path,
            )
            .map_err(|e| format!("failed to generate canonical patch: {e}"))
            .and_then(|_| ensure_patch_size(&patch_path, self.artifact_limits.patch_limit_bytes))
            .and_then(|_| {
                build_file_artifact_record(job.id.as_str(), "sandbox.patch", &patch_path)
            });

            match patch_result {
                Ok(record) => all_artifact_records.push(record),
                Err(err) if failure_reason.is_none() => failure_reason = Some(err),
                Err(_) => {}
            }
        } else if failure_reason.is_none() {
            failure_reason = collected.as_ref().err().cloned();
        }

        if let Err(err) = stop_result {
            if failure_reason.is_none() {
                failure_reason = Some(err);
            }
        }

        let artifact_refs = all_artifact_records
            .iter()
            .map(|artifact| ArtifactRef::new(artifact.artifact_ref.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RunJobError::Message(e.to_string()))?;
        job.collect_artifacts(artifact_refs)
            .map_err(|e| RunJobError::Message(e.to_string()))?;
        self.store
            .update_job_and_insert_artifacts(&job, &all_artifact_records)
            .map_err(|e| RunJobError::Message(e.to_string()))?;

        if let Some(reason) = failure_reason {
            job.mark_failed(reason.clone())
                .map_err(|e| RunJobError::Message(e.to_string()))?;
            jobs.update(&job)
                .map_err(|e| RunJobError::Message(e.to_string()))?;
            return Ok(RunJobOutcome {
                job_id: job.id.as_str().to_string(),
                state: job.state.clone(),
                failure_reason: Some(reason),
                logs_path,
                publish_warning: job.publish_warning.clone(),
            });
        }

        job.start_validation()
            .map_err(|e| RunJobError::Message(e.to_string()))?;
        jobs.update(&job)
            .map_err(|e| RunJobError::Message(e.to_string()))?;

        job.mark_validation_succeeded()
            .map_err(|e| RunJobError::Message(e.to_string()))?;
        jobs.update(&job)
            .map_err(|e| RunJobError::Message(e.to_string()))?;

        match publish_validated_changes(
            &mut job,
            &prepared,
            &all_artifact_records,
            resolve_publisher_config,
        ) {
            Ok(Some(event)) => {
                self.store
                    .update_job_and_insert_outbox(&job, &event)
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
            }
            Ok(None) => {
                jobs.update(&job)
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
            }
            Err(err) => {
                let reason = err.to_string();
                job.mark_failed(reason.clone())
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
                jobs.update(&job)
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
                return Ok(RunJobOutcome {
                    job_id: job.id.as_str().to_string(),
                    state: job.state.clone(),
                    failure_reason: Some(reason),
                    logs_path,
                    publish_warning: job.publish_warning.clone(),
                });
            }
        }

        job.mark_succeeded()
            .map_err(|e| RunJobError::Message(e.to_string()))?;
        jobs.update(&job)
            .map_err(|e| RunJobError::Message(e.to_string()))?;

        Ok(RunJobOutcome {
            job_id: job.id.as_str().to_string(),
            state: job.state.clone(),
            failure_reason: None,
            logs_path,
            publish_warning: job.publish_warning.clone(),
        })
    }
}

impl std::fmt::Display for RunJobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(job_id) => write!(f, "job not found: {job_id}"),
            Self::InvalidState { job_id, state } => write!(
                f,
                "job {} is not runnable from state {}",
                job_id,
                state.as_str()
            ),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for RunJobError {}

fn publish_validated_changes<F>(
    job: &mut Job,
    prepared: &PreparedWorkspace,
    artifact_records: &[NewArtifactRecord],
    resolve_publisher_config: F,
) -> Result<Option<NewOutboxEvent>, RunJobError>
where
    F: FnOnce(&Job) -> Result<PublishExecutionPlan, RunJobError>,
{
    match job.publish_policy {
        PublishPolicy::Never => {
            job.mark_publish_skipped()
                .map_err(|e| RunJobError::Message(e.to_string()))?;
            Ok(None)
        }
        PublishPolicy::OnValidationSuccess => match resolve_publisher_config(job)? {
            PublishExecutionPlan::SkipWithWarning(warning) => {
                job.mark_publish_skipped_with_warning(warning)
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
                Ok(None)
            }
            PublishExecutionPlan::GitHub(publishing) => {
                let patch_record = artifact_records
                    .iter()
                    .find(|artifact| artifact.artifact_ref == "sandbox.patch")
                    .ok_or_else(|| {
                        RunJobError::Message(
                            "sandbox.patch artifact is required before publishing".to_string(),
                        )
                    })?;
                let publisher = GitHubPublisher::new(publishing);
                let published = publisher
                    .publish_patch(
                        job.id.as_str(),
                        &job.instruction,
                        &prepared.trusted_clone_dir,
                        Path::new(&patch_record.path),
                    )
                    .map_err(|e| {
                        RunJobError::Message(format!("failed to publish validated changes: {e}"))
                    })?;
                let event = job
                    .mark_pull_request_created(
                        published.branch_name.clone(),
                        published.pull_request_url.clone(),
                        published.pull_request_number,
                    )
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
                let publish_result = job.publish_result.as_ref().ok_or_else(|| {
                    RunJobError::Message(
                        "publish result missing after pull request creation".to_string(),
                    )
                })?;

                Ok(Some(build_pull_request_created_outbox_event(
                    job.id.as_str(),
                    event.event_type(),
                    publish_result,
                )?))
            }
            PublishExecutionPlan::GitLab(publishing) => {
                let patch_record = artifact_records
                    .iter()
                    .find(|artifact| artifact.artifact_ref == "sandbox.patch")
                    .ok_or_else(|| {
                        RunJobError::Message(
                            "sandbox.patch artifact is required before publishing".to_string(),
                        )
                    })?;
                let publisher = GitLabPublisher::new(publishing);
                let published = publisher
                    .publish_patch(
                        job.id.as_str(),
                        &job.instruction,
                        &prepared.trusted_clone_dir,
                        Path::new(&patch_record.path),
                    )
                    .map_err(|e| {
                        RunJobError::Message(format!("failed to publish validated changes: {e}"))
                    })?;
                let event = job
                    .mark_pull_request_created(
                        published.branch_name.clone(),
                        published.pull_request_url.clone(),
                        published.pull_request_number,
                    )
                    .map_err(|e| RunJobError::Message(e.to_string()))?;
                let publish_result = job.publish_result.as_ref().ok_or_else(|| {
                    RunJobError::Message(
                        "publish result missing after pull request creation".to_string(),
                    )
                })?;

                Ok(Some(build_pull_request_created_outbox_event(
                    job.id.as_str(),
                    event.event_type(),
                    publish_result,
                )?))
            }
        },
    }
}

fn build_pull_request_created_outbox_event(
    job_id: &str,
    event_type: &str,
    publish_result: &PublishResult,
) -> Result<NewOutboxEvent, RunJobError> {
    #[derive(serde::Serialize)]
    struct PullRequestPayload<'a> {
        branch_name: &'a str,
        pull_request_number: u64,
        pull_request_url: &'a str,
    }

    let payload = serde_json::to_string(&PullRequestPayload {
        branch_name: &publish_result.branch_name,
        pull_request_number: publish_result.pull_request_number,
        pull_request_url: &publish_result.pull_request_url,
    })
    .map_err(|e| RunJobError::Message(format!("failed to serialize pull request payload: {e}")))?;

    Ok(NewOutboxEvent {
        event_id: format!("{job_id}-pr-created"),
        job_id: job_id.to_string(),
        event_type: event_type.to_string(),
        payload,
        status: OutboxStatus::Pending,
    })
}

fn build_workspace_artifact_records(
    job_id: &str,
    prepared: &PreparedWorkspace,
) -> Result<Vec<NewArtifactRecord>, String> {
    Ok(vec![
        build_workspace_artifact_record(
            job_id,
            "workspace.trusted_clone",
            &prepared.trusted_clone_dir,
        )?,
        build_workspace_artifact_record(
            job_id,
            "workspace.sandbox_input",
            &prepared.sandbox_workspace_dir,
        )?,
    ])
}

fn build_workspace_result_artifact_records(
    job_id: &str,
    collected: &CollectedExecutionOutput,
) -> Result<Vec<NewArtifactRecord>, String> {
    Ok(vec![build_workspace_artifact_record(
        job_id,
        "workspace.sandbox_result",
        &collected.modified_workspace_dir,
    )?])
}

fn build_workspace_artifact_record(
    job_id: &str,
    artifact_ref: &str,
    path: &Path,
) -> Result<NewArtifactRecord, String> {
    let fingerprint = fingerprint_tree(path)?;

    Ok(NewArtifactRecord {
        job_id: job_id.to_string(),
        artifact_ref: artifact_ref.to_string(),
        kind: artifact_ref.to_string(),
        path: path.display().to_string(),
        content_hash: fingerprint.content_hash,
        size_bytes: fingerprint.size_bytes,
    })
}

fn build_file_artifact_records(
    job_id: &str,
    inputs: &[(&str, &Path)],
) -> Result<Vec<NewArtifactRecord>, String> {
    inputs
        .iter()
        .map(|(artifact_ref, path)| build_file_artifact_record(job_id, artifact_ref, path))
        .collect()
}

fn build_file_artifact_record(
    job_id: &str,
    artifact_ref: &str,
    path: &Path,
) -> Result<NewArtifactRecord, String> {
    let contents = fs::read(path)
        .map_err(|e| format!("failed to read artifact file {}: {e}", path.display()))?;
    let mut hasher = Fnv1a::new();
    hasher.update(&contents);

    Ok(NewArtifactRecord {
        job_id: job_id.to_string(),
        artifact_ref: artifact_ref.to_string(),
        kind: artifact_ref.to_string(),
        path: path.display().to_string(),
        content_hash: hasher.finish_hex(),
        size_bytes: i64::try_from(contents.len())
            .map_err(|_| format!("artifact file too large: {}", path.display()))?,
    })
}

fn prepare_text_artifact(path: &Path, limit_bytes: usize) -> Result<(), String> {
    const TRUNCATION_MARKER: &str = "\n...[truncated by openoman]\n";

    let contents = fs::read(path)
        .map_err(|e| format!("failed to read artifact file {}: {e}", path.display()))?;
    if contents.len() <= limit_bytes {
        return Ok(());
    }

    let marker = TRUNCATION_MARKER.as_bytes();
    let keep_len = limit_bytes.saturating_sub(marker.len());
    let mut truncated = contents[..keep_len].to_vec();
    truncated.extend_from_slice(marker);
    fs::write(path, truncated)
        .map_err(|e| format!("failed to write truncated artifact {}: {e}", path.display()))
}

fn ensure_patch_size(path: &Path, patch_limit_bytes: u64) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|e| format!("failed to stat patch artifact {}: {e}", path.display()))?;
    if metadata.len() > patch_limit_bytes {
        return Err(format!(
            "canonical patch exceeds {} bytes at {}",
            patch_limit_bytes,
            path.display()
        ));
    }
    Ok(())
}

struct TreeFingerprint {
    content_hash: String,
    size_bytes: i64,
}

fn fingerprint_tree(path: &Path) -> Result<TreeFingerprint, String> {
    let mut hasher = Fnv1a::new();
    let size_bytes = fingerprint_entry(path, path, &mut hasher)?;
    Ok(TreeFingerprint {
        content_hash: hasher.finish_hex(),
        size_bytes,
    })
}

fn fingerprint_entry(root: &Path, path: &Path, hasher: &mut Fnv1a) -> Result<i64, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to read metadata for {}: {e}", path.display()))?;
    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative_display = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.display().to_string()
    };

    hasher.update(b"path:");
    hasher.update(relative_display.as_bytes());
    hasher.update(b"\n");

    if metadata.is_dir() {
        hasher.update(b"type:dir\n");
        let mut entries = fs::read_dir(path)
            .map_err(|e| format!("failed to list directory {}: {e}", path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("failed to read directory {}: {e}", path.display()))?;
        entries.sort_by_key(|entry| entry.file_name());

        let mut total_size = 0_i64;
        for entry in entries {
            total_size += fingerprint_entry(root, &entry.path(), hasher)?;
        }
        return Ok(total_size);
    }

    if metadata.is_file() {
        hasher.update(b"type:file\n");
        let contents =
            fs::read(path).map_err(|e| format!("failed to read file {}: {e}", path.display()))?;
        hasher.update(&contents);

        return i64::try_from(metadata.len())
            .map_err(|_| format!("file too large to track for artifact {}", path.display()));
    }

    hasher.update(b"type:other\n");
    Ok(0)
}

struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish_hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        process::Command,
        sync::{mpsc, Mutex},
        thread,
    };

    use tempfile::{NamedTempFile, TempDir};

    use super::*;
    use crate::{
        domain::{
            job::{RepoRef, Revision},
            request::CheckProfile,
        },
        execution::{
            CollectedExecutionOutput, ExecutionBackendCapabilities, ExecutionBackendKind,
            ExecutionError, ExecutionExitStatus, ExecutionHandle, ExecutionIsolation,
            ExecutionRunner, ResourceLimits,
        },
        persistence::ArtifactRecord,
    };

    struct WaitProbe {
        db_path: PathBuf,
        job_id: String,
        observed_tx: mpsc::Sender<(JobState, usize)>,
        continue_rx: mpsc::Receiver<()>,
    }

    struct FakeBackend {
        runtime_root: PathBuf,
        wait_status: ExecutionExitStatus,
        probe: Mutex<Option<WaitProbe>>,
    }

    struct FakeRunner {
        runtime_root: PathBuf,
        wait_status: ExecutionExitStatus,
        probe: Option<WaitProbe>,
        attempt: Option<FakeAttempt>,
    }

    struct FakeAttempt {
        run_dir: PathBuf,
        workspace_result_dir: PathBuf,
        report_path: PathBuf,
        logs_path: PathBuf,
    }

    impl FakeBackend {
        fn new(
            runtime_root: PathBuf,
            wait_status: ExecutionExitStatus,
            probe: Option<WaitProbe>,
        ) -> Self {
            Self {
                runtime_root,
                wait_status,
                probe: Mutex::new(probe),
            }
        }
    }

    impl crate::execution::ExecutionBackend for FakeBackend {
        fn kind(&self) -> ExecutionBackendKind {
            ExecutionBackendKind::Firecracker
        }

        fn capabilities(&self) -> ExecutionBackendCapabilities {
            ExecutionBackendCapabilities {
                isolation: ExecutionIsolation::MicroVm,
                requires_explicit_risk_acknowledgement: false,
                supports_host_proxy_networking: false,
            }
        }

        fn check_runtime_dependencies(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn create_runner(&self) -> Result<Box<dyn ExecutionRunner>, ExecutionError> {
            Ok(Box::new(FakeRunner {
                runtime_root: self.runtime_root.clone(),
                wait_status: self.wait_status.clone(),
                probe: self.probe.lock().expect("probe lock").take(),
                attempt: None,
            }))
        }
    }

    impl ExecutionRunner for FakeRunner {
        fn start(&mut self, spec: AttemptSpec) -> Result<ExecutionHandle, ExecutionError> {
            let run_dir = self
                .runtime_root
                .join("jobs")
                .join(&spec.job_id)
                .join(format!("attempt-{}", spec.attempt_id));
            let workspace_result_dir = run_dir.join("workspace-result");
            fs::create_dir_all(&workspace_result_dir)?;
            copy_tree_for_test(&spec.workspace_dir, &workspace_result_dir)?;

            let report_path = run_dir.join("report.txt");
            let logs_path = run_dir.join("logs.txt");
            self.attempt = Some(FakeAttempt {
                run_dir: run_dir.clone(),
                workspace_result_dir,
                report_path,
                logs_path,
            });

            Ok(ExecutionHandle { id: 1, run_dir })
        }

        fn wait(
            &mut self,
            _handle: &ExecutionHandle,
        ) -> Result<ExecutionExitStatus, ExecutionError> {
            let attempt = self
                .attempt
                .as_ref()
                .ok_or_else(|| ExecutionError::RunnerState("attempt missing".to_string()))?;

            if let Some(probe) = self.probe.take() {
                let store = SqliteStore::open(&probe.db_path)
                    .map_err(|e| ExecutionError::RunnerState(e.to_string()))?;
                let job_id = JobId::new(probe.job_id)
                    .map_err(|e| ExecutionError::RunnerState(e.to_string()))?;
                let job = store
                    .jobs()
                    .load(&job_id)
                    .map_err(|e| ExecutionError::RunnerState(e.to_string()))?
                    .ok_or_else(|| ExecutionError::RunnerState("job missing".to_string()))?;
                probe
                    .observed_tx
                    .send((job.state.clone(), job.attempts.len()))
                    .map_err(|e| ExecutionError::RunnerState(e.to_string()))?;
                probe
                    .continue_rx
                    .recv()
                    .map_err(|e| ExecutionError::RunnerState(e.to_string()))?;
            }

            let readme_path = attempt.workspace_result_dir.join("README.md");
            let mut readme = fs::read_to_string(&readme_path)?;
            readme.push('\n');
            fs::write(&readme_path, readme)?;
            fs::write(&attempt.report_path, "fake runner completed\n")?;
            fs::write(&attempt.logs_path, "fake runner logs\n")?;

            Ok(self.wait_status.clone())
        }

        fn collect_output(
            &self,
            _handle: &ExecutionHandle,
            _job_id: &str,
            _attempt_id: u32,
        ) -> Result<CollectedExecutionOutput, ExecutionError> {
            let attempt = self
                .attempt
                .as_ref()
                .ok_or_else(|| ExecutionError::RunnerState("attempt missing".to_string()))?;
            Ok(CollectedExecutionOutput {
                modified_workspace_dir: attempt.workspace_result_dir.clone(),
                report_path: attempt.report_path.clone(),
                logs_path: attempt.logs_path.clone(),
            })
        }

        fn stop(&mut self, handle: &ExecutionHandle) -> Result<(), ExecutionError> {
            let attempt = self
                .attempt
                .as_ref()
                .ok_or_else(|| ExecutionError::RunnerState("attempt missing".to_string()))?;
            if attempt.run_dir != handle.run_dir {
                return Err(ExecutionError::RunnerState(
                    "handle run dir mismatch".to_string(),
                ));
            }
            Ok(())
        }
    }

    #[test]
    fn run_job_persists_running_state_before_wait_completes() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        let trusted_root = temp.path().join("trusted");
        let runtime_root = temp.path().join("runtime");
        let db = NamedTempFile::new().expect("temp db");
        init_fixture_repo(&fixture_repo);

        let store = SqliteStore::open(db.path()).expect("open store");
        let (job, _) = Job::submit(
            JobId::new("job-running").expect("job id"),
            RepoRef::new(fixture_repo.display().to_string()).expect("repo ref"),
            None,
            Revision::new("main").expect("revision"),
            "append a blank line".to_string(),
            CheckProfile::new("unit").expect("profile"),
            PublishPolicy::Never,
        );
        store.jobs().create(&job).expect("create job");

        let (observed_tx, observed_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let db_path = db.path().to_path_buf();
        let trusted_root_clone = trusted_root.clone();
        let runtime_root_clone = runtime_root.clone();
        let job_id = job.id.as_str().to_string();

        let handle = thread::spawn(move || {
            let use_case = RunJobUseCase::new(
                SqliteStore::open(&db_path).expect("open store"),
                GitAdapter::new(&trusted_root_clone),
                Box::new(FakeBackend::new(
                    runtime_root_clone,
                    ExecutionExitStatus {
                        success: true,
                        code: Some(0),
                        timed_out: false,
                    },
                    Some(WaitProbe {
                        db_path: db_path.clone(),
                        job_id: job_id.clone(),
                        observed_tx,
                        continue_rx,
                    }),
                )),
                limits(),
                agent_execution(),
                None,
                None,
                RunJobArtifactLimits {
                    log_limit_bytes: 1024 * 1024,
                    report_limit_bytes: 256 * 1024,
                    patch_limit_bytes: 5 * 1024 * 1024,
                },
            );

            use_case.run(&JobId::new(job_id).expect("job id"), |_| {
                Ok(PublishExecutionPlan::SkipWithWarning(
                    "not used for publish_policy=never".to_string(),
                ))
            })
        });

        let (state, attempts) = observed_rx.recv().expect("observed state");
        assert_eq!(state, JobState::Running);
        assert_eq!(attempts, 1);
        continue_tx.send(()).expect("resume wait");

        let outcome = handle
            .join()
            .expect("thread join")
            .expect("run job outcome");
        assert_eq!(outcome.state, JobState::Succeeded);
        assert!(outcome.failure_reason.is_none());
        assert!(outcome.publish_warning.is_none());

        let reloaded = store
            .jobs()
            .load(&job.id)
            .expect("load job")
            .expect("job exists");
        assert_eq!(reloaded.state, JobState::Succeeded);
        assert_eq!(reloaded.attempts.len(), 1);
        assert_eq!(reloaded.artifacts.len(), 6);
    }

    #[test]
    fn run_job_marks_failed_after_unsuccessful_attempt_and_keeps_artifacts() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        let trusted_root = temp.path().join("trusted");
        let runtime_root = temp.path().join("runtime");
        let db = NamedTempFile::new().expect("temp db");
        init_fixture_repo(&fixture_repo);

        let store = SqliteStore::open(db.path()).expect("open store");
        let (job, _) = Job::submit(
            JobId::new("job-failing").expect("job id"),
            RepoRef::new(fixture_repo.display().to_string()).expect("repo ref"),
            None,
            Revision::new("main").expect("revision"),
            "append a blank line".to_string(),
            CheckProfile::new("unit").expect("profile"),
            PublishPolicy::Never,
        );
        store.jobs().create(&job).expect("create job");

        let use_case = RunJobUseCase::new(
            store.clone(),
            GitAdapter::new(&trusted_root),
            Box::new(FakeBackend::new(
                runtime_root,
                ExecutionExitStatus {
                    success: false,
                    code: Some(42),
                    timed_out: false,
                },
                None,
            )),
            limits(),
            agent_execution(),
            None,
            None,
            RunJobArtifactLimits {
                log_limit_bytes: 1024 * 1024,
                report_limit_bytes: 256 * 1024,
                patch_limit_bytes: 5 * 1024 * 1024,
            },
        );

        let outcome = use_case
            .run(&job.id, |_| {
                Ok(PublishExecutionPlan::SkipWithWarning(
                    "not used for publish_policy=never".to_string(),
                ))
            })
            .expect("run outcome should be returned");
        assert_eq!(outcome.state, JobState::Failed);
        assert_eq!(
            outcome.failure_reason.as_deref(),
            Some("sandbox attempt failed for job job-failing with exit code Some(42)")
        );

        let reloaded = store
            .jobs()
            .load(&job.id)
            .expect("load job")
            .expect("job exists");
        assert_eq!(reloaded.state, JobState::Failed);
        assert_eq!(reloaded.attempts.len(), 1);
        assert_eq!(reloaded.artifacts.len(), 6);

        let artifacts = store
            .artifacts()
            .list_by_job(job.id.as_str())
            .expect("artifacts");
        assert!(has_artifact(&artifacts, "sandbox.patch"));
        assert!(has_artifact(&artifacts, "sandbox.logs"));
    }

    #[test]
    fn run_job_succeeds_with_publish_warning_when_publish_is_skipped() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        let trusted_root = temp.path().join("trusted");
        let runtime_root = temp.path().join("runtime");
        let db = NamedTempFile::new().expect("temp db");
        init_fixture_repo(&fixture_repo);

        let store = SqliteStore::open(db.path()).expect("open store");
        let (mut job, _) = Job::submit(
            JobId::new("job-publish-warning").expect("job id"),
            RepoRef::new(fixture_repo.display().to_string()).expect("repo ref"),
            Some("demo-alias".to_string()),
            Revision::new("main").expect("revision"),
            "append a blank line".to_string(),
            CheckProfile::new("unit").expect("profile"),
            PublishPolicy::OnValidationSuccess,
        );
        store.jobs().create(&job).expect("create job");

        let use_case = RunJobUseCase::new(
            store.clone(),
            GitAdapter::new(&trusted_root),
            Box::new(FakeBackend::new(
                runtime_root,
                ExecutionExitStatus {
                    success: true,
                    code: Some(0),
                    timed_out: false,
                },
                None,
            )),
            limits(),
            agent_execution(),
            None,
            None,
            RunJobArtifactLimits {
                log_limit_bytes: 1024 * 1024,
                report_limit_bytes: 256 * 1024,
                patch_limit_bytes: 5 * 1024 * 1024,
            },
        );

        let outcome = use_case
            .run(&job.id, |_| {
                Ok(PublishExecutionPlan::SkipWithWarning(
                    "publishing skipped in test".to_string(),
                ))
            })
            .expect("run outcome should be returned");
        assert_eq!(outcome.state, JobState::Succeeded);
        assert!(outcome.failure_reason.is_none());
        assert_eq!(
            outcome.publish_warning.as_deref(),
            Some("publishing skipped in test")
        );

        job = store
            .jobs()
            .load(&job.id)
            .expect("load job")
            .expect("job exists");
        assert_eq!(job.state, JobState::Succeeded);
        assert_eq!(
            job.publish_warning.as_deref(),
            Some("publishing skipped in test")
        );
        assert!(job.publish_result.is_none());
    }

    fn limits() -> ResourceLimits {
        ResourceLimits {
            vcpu_count: 1,
            memory_mib: 256,
            disk_quota_bytes: 1024 * 1024,
            timeout_secs: 30,
        }
    }

    fn agent_execution() -> AgentExecutionSpec {
        AgentExecutionSpec {
            provider: "codex".to_string(),
            bin: "codex".to_string(),
            model: None,
            auth_file: None,
            api_key: None,
            egress_proxy: None,
            egress_allowed_domains: Vec::new(),
        }
    }

    fn has_artifact(artifacts: &[ArtifactRecord], artifact_ref: &str) -> bool {
        artifacts
            .iter()
            .any(|artifact| artifact.artifact_ref == artifact_ref)
    }

    fn init_fixture_repo(repo_dir: &Path) {
        fs::create_dir_all(repo_dir).expect("create repo dir");
        git(
            None,
            [
                OsStr::new("init"),
                OsStr::new("--quiet"),
                OsStr::new("--initial-branch=main"),
                repo_dir.as_os_str(),
            ],
        );
        git(
            Some(repo_dir),
            [
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("OpenOMAN Test"),
            ],
        );
        git(
            Some(repo_dir),
            [
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("test@openoman.invalid"),
            ],
        );
        fs::write(repo_dir.join("README.md"), "hello\n").expect("write readme");
        fs::write(repo_dir.join("notes.txt"), "notes\n").expect("write notes");
        git(Some(repo_dir), [OsStr::new("add"), OsStr::new(".")]);
        git(
            Some(repo_dir),
            [
                OsStr::new("commit"),
                OsStr::new("--quiet"),
                OsStr::new("-m"),
                OsStr::new("initial"),
            ],
        );
    }

    fn git<'a>(current_dir: Option<&Path>, args: impl IntoIterator<Item = &'a OsStr>) {
        let mut command = Command::new("git");
        command.args(args);
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }
        let output = command.output().expect("run git");
        if !output.status.success() {
            panic!(
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }

    fn copy_tree_for_test(source: &Path, destination: &Path) -> Result<(), ExecutionError> {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree_for_test(&source_path, &destination_path)?;
            } else if entry.file_type()?.is_file() {
                fs::copy(source_path, destination_path)?;
            }
        }
        Ok(())
    }
}
