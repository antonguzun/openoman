use std::path::Path;

use clap::Parser;

use crate::{
    cli::{Cli, Commands},
    config::AppConfig,
    internal::run_internal,
    server,
    service::{OperatorService, SubmitJobInput},
};

pub(crate) async fn run() -> Result<(), String> {
    run_cli(Cli::parse()).await
}

async fn run_cli(cli: Cli) -> Result<(), String> {
    let Cli { config, command } = cli;
    if let Commands::Internal { command } = command {
        return run_internal(command);
    }

    let service = load_service(&config)?;
    dispatch_command(command, service).await
}

fn load_service(config_path: &Path) -> Result<OperatorService, String> {
    OperatorService::new(AppConfig::load(config_path)?)
}

async fn dispatch_command(command: Commands, service: OperatorService) -> Result<(), String> {
    match command {
        Commands::Serve => server::serve(service).await,
        Commands::Submit {
            repo,
            revision,
            instruction,
            check_profile,
            publish_policy,
        } => {
            let job = service.submit_job(SubmitJobInput {
                repo,
                revision,
                instruction,
                check_profile,
                publish_policy,
            })?;
            println!("job_id={}", job.job_id);
            Ok(())
        }
        Commands::Run { job_id } => {
            let run = service.run_job(&job_id)?;
            if let Some(reason) = run.failure_reason {
                return Err(reason);
            }
            if let Some(warning) = run.publish_warning {
                println!("publish_warning={warning}");
            }
            println!("job {} finished with state={}", run.job_id, run.state);
            Ok(())
        }
        Commands::Status { job_id } => {
            let job = service.get_job(&job_id)?;
            println!("job_id={}", job.job_id);
            println!("state={}", job.state);
            println!("attempts={}", job.attempts);
            Ok(())
        }
        Commands::Logs { job_id } => {
            let logs = service.get_logs(&job_id)?;
            if let Some(contents) = logs.contents {
                print!("{contents}");
                return Ok(());
            }
            if logs.events.is_empty() {
                println!("no logs for {job_id}");
                return Ok(());
            }
            for event in logs.events {
                println!("{} {} {}", event.event_id, event.event_type, event.status);
            }
            Ok(())
        }
        Commands::Artifacts { job_id } => {
            let artifacts = service.list_artifacts(&job_id)?;
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
        Commands::Result { job_id } => {
            let result = service.get_result(&job_id)?;
            println!("job_id={} result={}", result.job_id, result.result);
            if let Some(publish_result) = result.publish_result {
                println!("branch={}", publish_result.branch_name);
                println!("pull_request_number={}", publish_result.pull_request_number);
                println!("pull_request_url={}", publish_result.pull_request_url);
            }
            if let Some(warning) = result.publish_warning {
                println!("publish_warning={warning}");
            }
            Ok(())
        }
        Commands::Internal { .. } => unreachable!("internal commands are handled before config"),
    }
}
