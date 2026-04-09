use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use openoman_core::{
    application::{PublishExecutionPlan, RunJobArtifactLimits, RunJobError, RunJobUseCase},
    domain::{
        job::{Job, JobId, JobState, RepoRef, Revision},
        request::{CheckProfile, PublishPolicy},
    },
    execution::{build_execution_backend, ExecutionBackend},
    git::GitAdapter,
    persistence::{ArtifactRecord, NewOutboxEvent, OutboxEventRecord, OutboxStatus, SqliteStore},
};
use serde::Serialize;

use crate::config::{
    ensure_network_privileges_guard, resolve_agent_execution_spec, AppConfig, PublishRuntimePlan,
};
use crate::launcher;

const LOG_LIMIT_BYTES: usize = 1024 * 1024;
const REPORT_LIMIT_BYTES: usize = 256 * 1024;
const PATCH_LIMIT_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct OperatorService {
    config: AppConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct SubmitJobInput {
    pub(crate) repo: String,
    pub(crate) revision: String,
    pub(crate) instruction: String,
    pub(crate) check_profile: String,
    pub(crate) publish_policy: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobView {
    pub(crate) job_id: String,
    pub(crate) repo_ref: String,
    pub(crate) repo_alias: Option<String>,
    pub(crate) branch_name: Option<String>,
    pub(crate) commit_message: Option<String>,
    pub(crate) revision: String,
    pub(crate) instruction: String,
    pub(crate) check_profile: String,
    pub(crate) publish_policy: String,
    pub(crate) state: String,
    pub(crate) attempts: usize,
    pub(crate) artifact_refs: Vec<String>,
    pub(crate) publish_warning: Option<String>,
    pub(crate) publish_result: Option<PublishResultView>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublishResultView {
    pub(crate) branch_name: String,
    pub(crate) pull_request_url: String,
    pub(crate) pull_request_number: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobLogsView {
    pub(crate) job_id: String,
    pub(crate) source: String,
    pub(crate) contents: Option<String>,
    pub(crate) events: Vec<OutboxEventView>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OutboxEventView {
    pub(crate) event_id: String,
    pub(crate) event_type: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ArtifactView {
    pub(crate) artifact_ref: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) content_hash: String,
    pub(crate) size_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobResultView {
    pub(crate) job_id: String,
    pub(crate) status: String,
    pub(crate) result: String,
    pub(crate) branch_name: Option<String>,
    pub(crate) branch_url: Option<String>,
    pub(crate) commit_message: Option<String>,
    pub(crate) publish_warning: Option<String>,
    pub(crate) merge_request_url: Option<String>,
    pub(crate) publish_result: Option<PublishResultView>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub(crate) struct RunJobView {
    pub(crate) job_id: String,
    pub(crate) state: String,
    pub(crate) failure_reason: Option<String>,
    pub(crate) publish_warning: Option<String>,
}

impl OperatorService {
    pub(crate) fn for_api(config: AppConfig) -> Result<Self, String> {
        Self::from_config(config, false)
    }

    pub(crate) fn for_launcher(config: AppConfig) -> Result<Self, String> {
        Self::from_config(config, true)
    }

    pub(crate) fn config(&self) -> &AppConfig {
        &self.config
    }

    pub(crate) fn submit_job(&self, input: SubmitJobInput) -> Result<JobView, String> {
        let store = self.open_store()?;
        let id = JobId::new(generate_job_id()).map_err(|e| e.to_string())?;
        let resolved_repo = self.config.resolve_submit_repo(&input.repo)?;
        let repo_ref = RepoRef::new(resolved_repo.repo_ref).map_err(|e| e.to_string())?;
        let revision = Revision::new(input.revision).map_err(|e| e.to_string())?;
        let check_profile = CheckProfile::new(input.check_profile).map_err(|e| e.to_string())?;
        let publish_policy =
            PublishPolicy::parse(&input.publish_policy).map_err(|e| e.to_string())?;
        let git_naming = self.config.resolve_submit_git_naming(
            id.as_str(),
            repo_ref.as_str(),
            resolved_repo.repo_alias.as_deref(),
            &revision,
            &input.instruction,
        );

        let (job, event) = Job::submit(
            id,
            repo_ref,
            resolved_repo.repo_alias,
            Some(git_naming.branch_name),
            Some(git_naming.commit_message),
            revision,
            input.instruction,
            check_profile,
            publish_policy,
        )
        .map_err(|e| e.to_string())?;
        store.jobs().create(&job).map_err(|e| e.to_string())?;
        store
            .outbox()
            .insert(&NewOutboxEvent {
                event_id: format!("{}-submitted", job.id.as_str()),
                job_id: job.id.as_str().to_string(),
                event_type: event.event_type().to_string(),
                payload: "{}".to_string(),
                status: OutboxStatus::Pending,
            })
            .map_err(|e| e.to_string())?;

        Ok(job_to_view(&job))
    }

    pub(crate) fn list_jobs(&self) -> Result<Vec<JobView>, String> {
        let store = self.open_store()?;
        let jobs = store.jobs().list().map_err(|e| e.to_string())?;
        Ok(jobs.iter().map(job_to_view).collect())
    }

    pub(crate) fn get_job(&self, job_id: &str) -> Result<JobView, String> {
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let Some(mut job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };
        self.ensure_job_git_naming(&store, &mut job)?;
        Ok(job_to_view(&job))
    }

    pub(crate) fn retry_job(&self, job_id: &str) -> Result<JobView, String> {
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let Some(job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };

        let (retry, event) = Job::submit(
            JobId::new(generate_job_id()).map_err(|e| e.to_string())?,
            job.repo_ref.clone(),
            job.repo_alias.clone(),
            job.branch_name.clone(),
            job.commit_message.clone(),
            job.revision.clone(),
            job.instruction.clone(),
            job.check_profile.clone(),
            job.publish_policy.clone(),
        )
        .map_err(|e| e.to_string())?;
        store.jobs().create(&retry).map_err(|e| e.to_string())?;
        store
            .outbox()
            .insert(&NewOutboxEvent {
                event_id: format!("{}-submitted", retry.id.as_str()),
                job_id: retry.id.as_str().to_string(),
                event_type: event.event_type().to_string(),
                payload: "{}".to_string(),
                status: OutboxStatus::Pending,
            })
            .map_err(|e| e.to_string())?;

        Ok(job_to_view(&retry))
    }

    pub(crate) fn next_queued_job_id(&self) -> Result<Option<String>, String> {
        let store = self.open_store()?;
        let jobs = store.jobs().list().map_err(|e| e.to_string())?;
        Ok(jobs
            .into_iter()
            .rev()
            .find(|job| job.state == JobState::Queued)
            .map(|job| job.id.as_str().to_string()))
    }

    pub(crate) fn ensure_launcher_available(&self) -> Result<(), String> {
        launcher::health(&self.config.launcher.socket_path)
    }

    pub(crate) fn dispatch_job_to_launcher(&self, job_id: &str) -> Result<RunJobView, String> {
        launcher::run_job(&self.config.launcher.socket_path, job_id)
    }

    pub(crate) fn run_job(&self, job_id: &str) -> Result<RunJobView, String> {
        let _network_privileges = ensure_network_privileges_guard(&self.config.execution)?;
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let Some(mut submitted_job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };
        self.ensure_job_git_naming(&store, &mut submitted_job)?;
        let execution_backend = self.execution_backend()?;
        let agent_execution = resolve_agent_execution_spec(&self.config.agent)?;
        let repo_env_source_dir = self
            .config
            .env_overlay_dir_for_alias(submitted_job.repo_alias.as_deref());
        let repo_clone_token = self
            .config
            .resolve_clone_token_for_job(submitted_job.repo_alias.as_deref());
        let repo_post_clone_command = self
            .config
            .post_clone_command_for_alias(submitted_job.repo_alias.as_deref());
        let run_job = RunJobUseCase::new(
            store,
            GitAdapter::new(&self.config.trusted_workspace_dir),
            execution_backend,
            self.config.execution.limits.clone(),
            agent_execution,
            repo_env_source_dir,
            repo_clone_token,
            repo_post_clone_command,
            RunJobArtifactLimits {
                log_limit_bytes: LOG_LIMIT_BYTES,
                report_limit_bytes: REPORT_LIMIT_BYTES,
                patch_limit_bytes: PATCH_LIMIT_BYTES,
            },
        );
        let outcome = run_job
            .run(&job_id, |job| {
                let resolved = self
                    .config
                    .resolve_publish_plan_for_job(job.repo_alias.as_deref(), &job.revision)
                    .map_err(RunJobError::Message)?;
                match resolved {
                    PublishRuntimePlan::GitHub(config) => Ok(PublishExecutionPlan::GitHub(config)),
                    PublishRuntimePlan::GitLab(config) => Ok(PublishExecutionPlan::GitLab(config)),
                    PublishRuntimePlan::SkipWithWarning(warning) => {
                        Ok(PublishExecutionPlan::SkipWithWarning(warning))
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        if let Some(reason) = &outcome.failure_reason {
            if let Some(logs_path) = outcome.logs_path.as_deref() {
                print_sandbox_logs_to_stderr(logs_path);
            }
            return Ok(RunJobView {
                job_id: outcome.job_id,
                state: outcome.state.as_str().to_string(),
                failure_reason: Some(reason.clone()),
                publish_warning: outcome.publish_warning,
            });
        }

        Ok(RunJobView {
            job_id: outcome.job_id,
            state: outcome.state.as_str().to_string(),
            failure_reason: None,
            publish_warning: outcome.publish_warning,
        })
    }

    pub(crate) fn get_logs(&self, job_id: &str) -> Result<JobLogsView, String> {
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let job_id_str = job_id.as_str().to_string();
        let artifacts = store
            .artifacts()
            .list_by_job(job_id.as_str())
            .map_err(|e| e.to_string())?;
        if let Some(log_artifact) = artifacts
            .iter()
            .find(|artifact| artifact.artifact_ref == "sandbox.logs")
        {
            let contents = fs::read_to_string(&log_artifact.path)
                .map_err(|e| format!("failed to read sandbox logs {}: {e}", log_artifact.path))?;
            return Ok(JobLogsView {
                job_id: job_id_str,
                source: "artifact".to_string(),
                contents: Some(contents),
                events: vec![],
            });
        }

        let events = store
            .outbox()
            .list_by_job(job_id.as_str())
            .map_err(|e| e.to_string())?;
        if events.is_empty() {
            return Ok(JobLogsView {
                job_id: job_id_str,
                source: "none".to_string(),
                contents: None,
                events: vec![],
            });
        }

        Ok(JobLogsView {
            job_id: job_id_str,
            source: "outbox".to_string(),
            contents: None,
            events: events.iter().map(outbox_event_to_view).collect(),
        })
    }

    pub(crate) fn list_artifacts(&self, job_id: &str) -> Result<Vec<ArtifactView>, String> {
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let artifacts = store
            .artifacts()
            .list_by_job(job_id.as_str())
            .map_err(|e| e.to_string())?;
        Ok(artifacts.iter().map(artifact_to_view).collect())
    }

    pub(crate) fn get_result(&self, job_id: &str) -> Result<JobResultView, String> {
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let Some(mut job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };
        self.ensure_job_git_naming(&store, &mut job)?;

        let result = match job.state {
            JobState::Succeeded => "success",
            JobState::Failed => "failed",
            JobState::Canceled => "canceled",
            _ => "in_progress",
        };
        let branch_url = self.config.resolve_result_branch_url(
            job.repo_alias.as_deref(),
            job.repo_ref.as_str(),
            job.branch_name.as_deref(),
        );
        let merge_request_url = job
            .publish_result
            .as_ref()
            .map(|publish_result| publish_result.pull_request_url.clone());
        Ok(JobResultView {
            job_id: job.id.as_str().to_string(),
            status: job.state.as_str().to_string(),
            result: result.to_string(),
            branch_name: job.branch_name.clone(),
            branch_url,
            commit_message: job.commit_message.clone(),
            publish_warning: job.publish_warning.clone(),
            merge_request_url,
            publish_result: job.publish_result.as_ref().map(publish_result_to_view),
        })
    }

    fn from_config(config: AppConfig, validate_execution_backend: bool) -> Result<Self, String> {
        if validate_execution_backend {
            let execution_backend = build_execution_backend(config.execution.clone())
                .map_err(|e| format!("failed to configure sandbox backend: {e}"))?;
            execution_backend
                .check_runtime_dependencies()
                .map_err(|e| format!("sandbox backend validation failed: {e}"))?;
        }
        SqliteStore::open(&config.database_path).map_err(|e| {
            format!(
                "failed to open sqlite store at {}: {e}",
                config.database_path.display()
            )
        })?;

        Ok(Self { config })
    }

    fn open_store(&self) -> Result<SqliteStore, String> {
        SqliteStore::open(&self.config.database_path).map_err(|e| {
            format!(
                "failed to open sqlite store at {}: {e}",
                self.config.database_path.display()
            )
        })
    }

    fn execution_backend(&self) -> Result<Box<dyn ExecutionBackend>, String> {
        let execution_backend = build_execution_backend(self.config.execution.clone())
            .map_err(|e| format!("failed to configure sandbox backend: {e}"))?;
        execution_backend
            .check_runtime_dependencies()
            .map_err(|e| format!("sandbox backend validation failed: {e}"))?;
        Ok(execution_backend)
    }

    fn ensure_job_git_naming(&self, store: &SqliteStore, job: &mut Job) -> Result<(), String> {
        if job.branch_name.is_some() && job.commit_message.is_some() {
            return Ok(());
        }

        let naming = self
            .config
            .resolve_legacy_git_naming_for_job(job.id.as_str(), job.repo_alias.as_deref());
        job.ensure_git_naming(naming.branch_name, naming.commit_message)
            .map_err(|e| e.to_string())?;
        store.jobs().update(job).map_err(|e| e.to_string())
    }
}

fn parse_job_id(job_id: &str) -> Result<JobId, String> {
    JobId::new(job_id.to_string()).map_err(|e| e.to_string())
}

fn outbox_event_to_view(event: &OutboxEventRecord) -> OutboxEventView {
    OutboxEventView {
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        status: event.status.as_str().to_string(),
    }
}

fn artifact_to_view(artifact: &ArtifactRecord) -> ArtifactView {
    ArtifactView {
        artifact_ref: artifact.artifact_ref.clone(),
        kind: artifact.kind.clone(),
        path: artifact.path.clone(),
        content_hash: artifact.content_hash.clone(),
        size_bytes: artifact.size_bytes,
    }
}

fn job_to_view(job: &Job) -> JobView {
    JobView {
        job_id: job.id.as_str().to_string(),
        repo_ref: job.repo_ref.as_str().to_string(),
        repo_alias: job.repo_alias.clone(),
        branch_name: job.branch_name.clone(),
        commit_message: job.commit_message.clone(),
        revision: job.revision.as_str().to_string(),
        instruction: job.instruction.clone(),
        check_profile: job.check_profile.as_str().to_string(),
        publish_policy: job.publish_policy.as_str().to_string(),
        state: job.state.as_str().to_string(),
        attempts: job.attempts.len(),
        artifact_refs: job
            .artifacts
            .iter()
            .map(|artifact| artifact.as_str().to_string())
            .collect(),
        publish_warning: job.publish_warning.clone(),
        publish_result: job.publish_result.as_ref().map(publish_result_to_view),
    }
}

fn publish_result_to_view(
    publish_result: &openoman_core::domain::job::PublishResult,
) -> PublishResultView {
    PublishResultView {
        branch_name: publish_result.branch_name.clone(),
        pull_request_url: publish_result.pull_request_url.clone(),
        pull_request_number: publish_result.pull_request_number,
    }
}

fn generate_job_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("job-{now}")
}

pub(crate) fn print_sandbox_logs_to_stderr(path: &Path) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    if contents.is_empty() {
        return;
    }

    eprintln!("sandbox logs:");
    eprint!("{contents}");
    if !contents.ends_with('\n') {
        eprintln!();
    }
}
