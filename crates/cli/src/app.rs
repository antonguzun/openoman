#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitStatus,
    process::{Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};

use clap::Parser;
use tokio::{io::AsyncReadExt, process::Command};

use crate::{
    cli::{Cli, Commands, StackCommands},
    config::AppConfig,
    internal::run_internal,
    launcher, server,
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

    dispatch_command(command, config).await
}

fn load_api_service(config_path: &Path) -> Result<OperatorService, String> {
    OperatorService::for_api(AppConfig::load(config_path)?)
}

fn load_launcher_service(config_path: &Path) -> Result<OperatorService, String> {
    OperatorService::for_launcher(AppConfig::load(config_path)?)
}

async fn dispatch_command(command: Commands, config_path: PathBuf) -> Result<(), String> {
    match command {
        Commands::Serve | Commands::Api => server::serve(load_api_service(&config_path)?).await,
        Commands::Launcher => launcher::serve(load_launcher_service(&config_path)?).await,
        Commands::Dev => run_dev_mode(&config_path).await,
        Commands::Stack { command } => run_stack_command(command),
        Commands::Submit {
            repo,
            revision,
            instruction,
            check_profile,
            publish_policy,
        } => {
            let service = load_launcher_service(&config_path)?;
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
            let service = load_launcher_service(&config_path)?;
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
            let service = load_launcher_service(&config_path)?;
            let job = service.get_job(&job_id)?;
            println!("job_id={}", job.job_id);
            println!("state={}", job.state);
            println!("attempts={}", job.attempts);
            Ok(())
        }
        Commands::Logs { job_id } => {
            let service = load_launcher_service(&config_path)?;
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
            let service = load_launcher_service(&config_path)?;
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
            let service = load_launcher_service(&config_path)?;
            let result = service.get_result(&job_id)?;
            println!("job_id={} result={}", result.job_id, result.result);
            println!("status={}", result.status);
            if let Some(branch_name) = result.branch_name {
                println!("branch_name={branch_name}");
            }
            if let Some(branch_url) = result.branch_url {
                println!("branch_url={branch_url}");
            }
            if let Some(commit_message) = result.commit_message {
                println!("commit_message={commit_message}");
            }
            if let Some(merge_request_url) = result.merge_request_url {
                println!("merge_request_url={merge_request_url}");
            }
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

async fn run_dev_mode(config_path: &Path) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("failed to resolve current executable: {e}"))?;
    let mut launcher_child = spawn_role_child(&current_exe, config_path, "launcher").await?;
    let launcher_config = AppConfig::load(config_path)?;
    wait_for_launcher(&launcher_config, Duration::from_secs(5)).await?;
    let mut api_child = spawn_role_child(&current_exe, config_path, "api").await?;

    tokio::select! {
        shutdown = wait_for_dev_shutdown_signal() => {
            shutdown?;
            shutdown_child("api", &mut api_child).await;
            shutdown_child("launcher", &mut launcher_child).await;
            Ok(())
        }
        status = launcher_child.wait() => {
            let status = status.map_err(|e| format!("failed to wait for launcher process: {e}"))?;
            if exited_on_sigint(status) {
                eprintln!("launcher process exited on SIGINT (signal 2), shutting down dev mode");
                shutdown_child("api", &mut api_child).await;
                return Ok(());
            }
            shutdown_child("api", &mut api_child).await;
            Err(format!("launcher exited unexpectedly with status {status}"))
        }
        status = api_child.wait() => {
            let status = status.map_err(|e| format!("failed to wait for api process: {e}"))?;
            if exited_on_sigint(status) {
                eprintln!("api process exited on SIGINT (signal 2), shutting down dev mode");
                shutdown_child("launcher", &mut launcher_child).await;
                return Ok(());
            }
            shutdown_child("launcher", &mut launcher_child).await;
            Err(format!("api exited unexpectedly with status {status}"))
        }
    }
}

async fn wait_for_dev_shutdown_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|e| format!("failed to listen for SIGINT: {e}"))?;
        sigint.recv().await;
        eprintln!("dev mode received SIGINT (signal 2), shutting down child processes");
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| format!("failed to listen for Ctrl-C: {e}"))?;
        eprintln!("dev mode received Ctrl-C, shutting down child processes");
        Ok(())
    }
}

async fn spawn_role_child(
    current_exe: &Path,
    config_path: &Path,
    role: &str,
) -> Result<tokio::process::Child, String> {
    let mut command = Command::new(current_exe);
    command
        .arg("--config")
        .arg(config_path)
        .arg(role)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn {role} process: {e}"))?;
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(pipe_logs(role.to_string(), false, stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(pipe_logs(role.to_string(), true, stderr));
    }
    Ok(child)
}

async fn pipe_logs<T>(label: String, stderr: bool, stream: T)
where
    T: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let prefix = format!("[{label}] ").into_bytes();
    if stderr {
        pipe_logs_to_writer(stream, std::io::stderr(), prefix).await;
    } else {
        pipe_logs_to_writer(stream, std::io::stdout(), prefix).await;
    }
}

async fn pipe_logs_to_writer<T, W>(mut stream: T, mut writer: W, prefix: Vec<u8>)
where
    T: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: Write,
{
    let mut at_line_start = true;
    let mut read_buf = [0_u8; 4096];
    let mut write_buf = Vec::with_capacity(4096 + prefix.len());
    loop {
        match stream.read(&mut read_buf).await {
            Ok(0) => break,
            Ok(read) => {
                append_prefixed_log_chunk(
                    &prefix,
                    &read_buf[..read],
                    &mut at_line_start,
                    &mut write_buf,
                );
                if writer.write_all(&write_buf).is_err() {
                    break;
                }
                if writer.flush().is_err() {
                    break;
                }
                write_buf.clear();
            }
            Err(_) => break,
        }
    }
}

fn append_prefixed_log_chunk(
    prefix: &[u8],
    chunk: &[u8],
    at_line_start: &mut bool,
    output: &mut Vec<u8>,
) {
    for &byte in chunk {
        if *at_line_start {
            output.extend_from_slice(prefix);
            *at_line_start = false;
        }
        output.push(byte);
        if byte == b'\n' {
            *at_line_start = true;
        }
    }
}

async fn wait_for_launcher(config: &AppConfig, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let socket_path = config.launcher.socket_path.clone();
        match tokio::task::spawn_blocking(move || launcher::health(&socket_path))
            .await
            .map_err(|e| format!("launcher health join failed: {e}"))?
        {
            Ok(()) => return Ok(()),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(format!("launcher did not become ready: {err}"));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn shutdown_child(label: &str, child: &mut tokio::process::Child) {
    if let Some(id) = child.id() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        eprintln!("{label} process {id} stopped");
    }
}

fn exited_on_sigint(status: ExitStatus) -> bool {
    #[cfg(unix)]
    {
        status.signal() == Some(2)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

fn run_stack_command(command: StackCommands) -> Result<(), String> {
    match command {
        StackCommands::Start => {
            run_systemctl(["start", "openoman-launcher.service", "openoman-api.service"])
        }
        StackCommands::Stop => {
            run_systemctl(["stop", "openoman-api.service", "openoman-launcher.service"])
        }
        StackCommands::Status => run_systemctl([
            "status",
            "openoman-launcher.service",
            "openoman-api.service",
        ]),
    }
}

fn run_systemctl<const N: usize>(args: [&str; N]) -> Result<(), String> {
    let output = ProcessCommand::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run systemctl {}: {e}", args.join(" ")))?;
    if !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "systemctl {} failed with status {}",
                args.join(" "),
                output.status
            )
        } else {
            format!("systemctl {} failed: {stderr}", args.join(" "))
        });
    }
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn exited_on_sigint_matches_signal_2_exit_status() {
        assert!(exited_on_sigint(ExitStatus::from_raw(2)));
    }

    #[cfg(unix)]
    #[test]
    fn exited_on_sigint_rejects_normal_process_exits() {
        assert!(!exited_on_sigint(ExitStatus::from_raw(0)));
        assert!(!exited_on_sigint(ExitStatus::from_raw(1 << 8)));
    }

    #[test]
    fn append_prefixed_log_chunk_prefixes_partial_writes_without_newline() {
        let mut at_line_start = true;
        let mut output = Vec::new();
        let prefix = b"[api] ";

        append_prefixed_log_chunk(prefix, b"sudo password:", &mut at_line_start, &mut output);
        append_prefixed_log_chunk(
            prefix,
            b" waiting\nnext line",
            &mut at_line_start,
            &mut output,
        );

        assert_eq!(
            String::from_utf8(output).expect("utf8"),
            "[api] sudo password: waiting\n[api] next line"
        );
        assert!(!at_line_start);
    }
}
