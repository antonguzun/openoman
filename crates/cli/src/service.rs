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
    ensure_network_privileges, resolve_agent_execution_spec, AppConfig, PublishRuntimePlan,
};

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
    pub(crate) result: String,
    pub(crate) publish_warning: Option<String>,
    pub(crate) publish_result: Option<PublishResultView>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunJobView {
    pub(crate) job_id: String,
    pub(crate) state: String,
    pub(crate) failure_reason: Option<String>,
    pub(crate) publish_warning: Option<String>,
}

impl OperatorService {
    pub(crate) fn new(config: AppConfig) -> Result<Self, String> {
        let execution_backend = build_execution_backend(config.execution.clone())
            .map_err(|e| format!("failed to configure sandbox backend: {e}"))?;
        execution_backend
            .check_runtime_dependencies()
            .map_err(|e| format!("sandbox backend validation failed: {e}"))?;
        SqliteStore::open(&config.database_path).map_err(|e| {
            format!(
                "failed to open sqlite store at {}: {e}",
                config.database_path.display()
            )
        })?;

        Ok(Self { config })
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

        let (job, event) = Job::submit(
            id,
            repo_ref,
            resolved_repo.repo_alias,
            revision,
            input.instruction,
            check_profile,
            publish_policy,
        );
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
        let Some(job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };
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
            job.revision.clone(),
            job.instruction.clone(),
            job.check_profile.clone(),
            job.publish_policy.clone(),
        );
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

    pub(crate) fn run_job(&self, job_id: &str) -> Result<RunJobView, String> {
        ensure_network_privileges(&self.config.execution)?;
        let store = self.open_store()?;
        let job_id = parse_job_id(job_id)?;
        let Some(submitted_job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };
        let execution_backend = self.execution_backend()?;
        let agent_execution = resolve_agent_execution_spec(&self.config.agent)?;
        let repo_env_source_dir = self
            .config
            .env_overlay_dir_for_alias(submitted_job.repo_alias.as_deref());
        let repo_clone_token = self
            .config
            .resolve_clone_token_for_job(submitted_job.repo_alias.as_deref());
        let run_job = RunJobUseCase::new(
            store,
            GitAdapter::new(&self.config.trusted_workspace_dir),
            execution_backend,
            self.config.execution.limits.clone(),
            agent_execution,
            repo_env_source_dir,
            repo_clone_token,
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
        let Some(job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {}", job_id.as_str()));
        };

        let result = match job.state {
            JobState::Succeeded => "success",
            JobState::Failed => "failed",
            JobState::Canceled => "canceled",
            _ => "in_progress",
        };
        Ok(JobResultView {
            job_id: job.id.as_str().to_string(),
            result: result.to_string(),
            publish_warning: job.publish_warning.clone(),
            publish_result: job.publish_result.as_ref().map(publish_result_to_view),
        })
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
