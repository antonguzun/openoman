use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use openoman_core::{
    application::{PublishExecutionPlan, RunJobArtifactLimits, RunJobError, RunJobUseCase},
    domain::{
        job::{Job, JobId, JobState, RepoRef, Revision},
        plugin::{CheckProfile, PublishPolicy},
    },
    execution::{build_execution_backend, ExecutionBackend},
    git::GitAdapter,
    persistence::{NewOutboxEvent, OutboxStatus, SqliteStore},
};

use crate::{
    cli::{Cli, Commands},
    config::{
        ensure_network_privileges, resolve_agent_execution_spec, AppConfig, PublishRuntimePlan,
    },
    internal::run_internal,
};

const LOG_LIMIT_BYTES: usize = 1024 * 1024;
const REPORT_LIMIT_BYTES: usize = 256 * 1024;
const PATCH_LIMIT_BYTES: u64 = 5 * 1024 * 1024;

struct RuntimeContext {
    config: AppConfig,
    execution_backend: Box<dyn ExecutionBackend>,
    store: SqliteStore,
}

pub(crate) fn run() -> Result<(), String> {
    run_cli(Cli::parse())
}

fn run_cli(cli: Cli) -> Result<(), String> {
    let Cli { config, command } = cli;
    if let Commands::Internal { command } = command {
        return run_internal(command);
    }

    let context = load_runtime(&config)?;
    dispatch_command(command, context)
}

fn load_runtime(config_path: &Path) -> Result<RuntimeContext, String> {
    let config = AppConfig::load(config_path)?;
    let execution_backend = build_execution_backend(config.execution.clone())
        .map_err(|e| format!("failed to configure sandbox backend: {e}"))?;
    execution_backend
        .check_runtime_dependencies()
        .map_err(|e| format!("sandbox backend validation failed: {e}"))?;
    let store = SqliteStore::open(&config.database_path).map_err(|e| {
        format!(
            "failed to open sqlite store at {}: {e}",
            config.database_path.display()
        )
    })?;

    Ok(RuntimeContext {
        config,
        execution_backend,
        store,
    })
}

fn dispatch_command(command: Commands, context: RuntimeContext) -> Result<(), String> {
    match command {
        Commands::Submit {
            repo,
            revision,
            instruction,
            check_profile,
            publish_policy,
        } => run_submit(
            &context.config,
            &context.store,
            repo,
            revision,
            instruction,
            check_profile,
            publish_policy,
        ),
        Commands::Run { job_id } => run_job(
            &context.config,
            context.execution_backend,
            &context.store,
            job_id,
        ),
        Commands::Status { job_id } => run_status(&context.store, &job_id),
        Commands::Logs { job_id } => run_logs(&context.store, &job_id),
        Commands::Artifacts { job_id } => run_artifacts(&context.store, &job_id),
        Commands::Result { job_id } => run_result(&context.store, &job_id),
        Commands::Internal { .. } => unreachable!("internal commands are handled before config"),
    }
}

fn run_submit(
    config: &AppConfig,
    store: &SqliteStore,
    repo: String,
    revision: String,
    instruction: String,
    check_profile: String,
    publish_policy: String,
) -> Result<(), String> {
    let id = JobId::new(generate_job_id()).map_err(|e| e.to_string())?;
    let resolved_repo = config.resolve_submit_repo(&repo)?;
    let repo_ref = RepoRef::new(resolved_repo.repo_ref).map_err(|e| e.to_string())?;
    let revision = Revision::new(revision).map_err(|e| e.to_string())?;
    let check_profile = CheckProfile::new(check_profile).map_err(|e| e.to_string())?;
    let publish_policy = PublishPolicy::parse(&publish_policy).map_err(|e| e.to_string())?;

    let (job, event) = Job::submit(
        id,
        repo_ref,
        resolved_repo.repo_alias,
        revision,
        instruction,
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

    println!("job_id={}", job.id.as_str());
    Ok(())
}

fn run_job(
    config: &AppConfig,
    execution_backend: Box<dyn ExecutionBackend>,
    store: &SqliteStore,
    job_id: String,
) -> Result<(), String> {
    ensure_network_privileges(&config.execution)?;
    let job_id = JobId::new(job_id).map_err(|e| e.to_string())?;
    let Some(submitted_job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
        return Err(format!("job not found: {}", job_id.as_str()));
    };
    let agent_execution = resolve_agent_execution_spec(&config.agent)?;
    let repo_env_source_dir = config.env_overlay_dir_for_alias(submitted_job.repo_alias.as_deref());
    let run_job = RunJobUseCase::new(
        store.clone(),
        GitAdapter::new(&config.trusted_workspace_dir),
        execution_backend,
        config.execution.limits.clone(),
        agent_execution,
        repo_env_source_dir,
        RunJobArtifactLimits {
            log_limit_bytes: LOG_LIMIT_BYTES,
            report_limit_bytes: REPORT_LIMIT_BYTES,
            patch_limit_bytes: PATCH_LIMIT_BYTES,
        },
    );
    let outcome = run_job
        .run(&job_id, |job| {
            let resolved = config
                .resolve_publish_plan_for_job(job.repo_alias.as_deref(), &job.revision)
                .map_err(RunJobError::Message)?;
            match resolved {
                PublishRuntimePlan::GitHub(config) => Ok(PublishExecutionPlan::GitHub(config)),
                PublishRuntimePlan::SkipWithWarning(warning) => {
                    Ok(PublishExecutionPlan::SkipWithWarning(warning))
                }
            }
        })
        .map_err(|e| e.to_string())?;

    if let Some(reason) = outcome.failure_reason {
        if let Some(logs_path) = outcome.logs_path.as_deref() {
            print_sandbox_logs_to_stderr(logs_path);
        }
        return Err(reason);
    }
    if let Some(warning) = outcome.publish_warning.as_deref() {
        println!("publish_warning={warning}");
    }

    println!(
        "job {} finished with state={}",
        outcome.job_id,
        outcome.state.as_str()
    );
    Ok(())
}

fn run_status(store: &SqliteStore, job_id: &str) -> Result<(), String> {
    let job_id = JobId::new(job_id.to_string()).map_err(|e| e.to_string())?;
    let Some(job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
        return Err(format!("job not found: {}", job_id.as_str()));
    };

    println!("job_id={}", job.id.as_str());
    println!("state={}", job.state.as_str());
    println!("attempts={}", job.attempts.len());
    Ok(())
}

fn run_logs(store: &SqliteStore, job_id: &str) -> Result<(), String> {
    let artifacts = store
        .artifacts()
        .list_by_job(job_id)
        .map_err(|e| e.to_string())?;
    if let Some(log_artifact) = artifacts
        .iter()
        .find(|artifact| artifact.artifact_ref == "sandbox.logs")
    {
        let contents = fs::read_to_string(&log_artifact.path)
            .map_err(|e| format!("failed to read sandbox logs {}: {e}", log_artifact.path))?;
        print!("{contents}");
        return Ok(());
    }

    let events = store
        .outbox()
        .list_by_job(job_id)
        .map_err(|e| e.to_string())?;
    if events.is_empty() {
        println!("no logs for {job_id}");
        return Ok(());
    }

    for event in events {
        println!(
            "{} {} {}",
            event.event_id,
            event.event_type,
            event.status.as_str()
        );
    }
    Ok(())
}

fn run_artifacts(store: &SqliteStore, job_id: &str) -> Result<(), String> {
    let artifacts = store
        .artifacts()
        .list_by_job(job_id)
        .map_err(|e| e.to_string())?;
    if artifacts.is_empty() {
        println!("no artifacts for {job_id}");
        return Ok(());
    }

    for artifact in artifacts {
        println!(
            "{} {} {} {}",
            artifact.artifact_ref, artifact.kind, artifact.path, artifact.size_bytes
        );
    }
    Ok(())
}

fn run_result(store: &SqliteStore, job_id: &str) -> Result<(), String> {
    let job_id = JobId::new(job_id.to_string()).map_err(|e| e.to_string())?;
    let Some(job) = store.jobs().load(&job_id).map_err(|e| e.to_string())? else {
        return Err(format!("job not found: {}", job_id.as_str()));
    };

    let result = match job.state {
        JobState::Succeeded => "success",
        JobState::Failed => "failed",
        JobState::Canceled => "canceled",
        _ => "in_progress",
    };
    println!("job_id={} result={}", job.id.as_str(), result);
    if let Some(publish_result) = &job.publish_result {
        println!("branch={}", publish_result.branch_name);
        println!("pull_request_number={}", publish_result.pull_request_number);
        println!("pull_request_url={}", publish_result.pull_request_url);
    }
    if let Some(warning) = &job.publish_warning {
        println!("publish_warning={warning}");
    }
    Ok(())
}

fn generate_job_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("job-{now}")
}

fn print_sandbox_logs_to_stderr(path: &Path) {
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
