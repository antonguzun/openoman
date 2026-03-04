use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use super::super::{CollectedExecutionOutput, ExecutionError};

pub(super) struct FirecrackerArtifactCollector;

impl FirecrackerArtifactCollector {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn collect_output(
        &self,
        image_path: &Path,
        output_dir: &Path,
        serial_log_path: &Path,
        network_log_path: &Path,
    ) -> Result<CollectedExecutionOutput, ExecutionError> {
        if output_dir.exists() {
            fs::remove_dir_all(output_dir)?;
        }
        fs::create_dir_all(output_dir)?;

        run_command_allowing_codes(
            "e2fsck",
            &["-fy".to_string(), image_path.display().to_string()],
            &[1, 2],
        )?;

        let modified_workspace_dir = dump_workspace_from_image(image_path, output_dir)?;
        let report_path = output_dir.join("report.txt");
        let logs_path = output_dir.join("logs.txt");

        if dump_file_from_image(image_path, "/openoman-output/report.txt", &report_path).is_err()
            || !report_path.exists()
        {
            fs::write(
                &report_path,
                "Sandbox execution completed without report output.\n",
            )?;
        }

        if dump_file_from_image(image_path, "/openoman-output/logs.txt", &logs_path).is_err()
            || !logs_path.exists()
        {
            if serial_log_path.exists() {
                fs::copy(serial_log_path, &logs_path)?;
            } else {
                fs::write(&logs_path, "sandbox execution did not produce logs\n")?;
            }
        }
        if network_log_path.exists() {
            append_host_network_log(&logs_path, network_log_path)?;
        }

        Ok(CollectedExecutionOutput {
            modified_workspace_dir,
            report_path,
            logs_path,
        })
    }

    pub(super) fn read_exit_code_marker(
        &self,
        image_path: &Path,
    ) -> Result<Option<i32>, ExecutionError> {
        let args = vec![
            "-R".to_string(),
            "cat /openoman-output/exit-code.txt".to_string(),
            image_path.display().to_string(),
        ];
        let output = Command::new("debugfs").args(&args).output()?;
        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(value) = stdout.lines().map(str::trim).find(|line| !line.is_empty()) else {
            return Ok(None);
        };
        let code = value.parse::<i32>().map_err(|err| {
            ExecutionError::RunnerState(format!("invalid exit-code marker '{value}': {err}"))
        })?;
        Ok(Some(code))
    }
}

fn dump_file_from_image(
    image_path: &Path,
    image_file_path: &str,
    host_path: &Path,
) -> Result<(), ExecutionError> {
    let args = vec![
        "-R".to_string(),
        format!("dump -p {image_file_path} {}", host_path.display()),
        image_path.display().to_string(),
    ];
    run_command("debugfs", &args)
}

fn dump_workspace_from_image(
    image_path: &Path,
    output_dir: &Path,
) -> Result<PathBuf, ExecutionError> {
    let parent = output_dir.join("workspace-dump");
    if parent.exists() {
        fs::remove_dir_all(&parent)?;
    }
    fs::create_dir_all(&parent)?;

    let args = vec![
        "-R".to_string(),
        format!("rdump /workspace {}", parent.display()),
        image_path.display().to_string(),
    ];
    run_command("debugfs", &args)?;

    let dumped_workspace = parent.join("workspace");
    if !dumped_workspace.exists() {
        return Err(ExecutionError::RunnerState(format!(
            "debugfs did not produce {}",
            dumped_workspace.display()
        )));
    }

    let final_path = output_dir.join("workspace-result");
    if final_path.exists() {
        fs::remove_dir_all(&final_path)?;
    }
    fs::rename(&dumped_workspace, &final_path)?;
    fs::remove_dir_all(&parent)?;
    Ok(final_path)
}

fn append_host_network_log(
    logs_path: &Path,
    network_log_path: &Path,
) -> Result<(), ExecutionError> {
    let network_log = fs::read_to_string(network_log_path)?;
    if network_log.is_empty() {
        return Ok(());
    }

    let existing = fs::read(logs_path)?;
    let mut logs_file = fs::OpenOptions::new().append(true).open(logs_path)?;
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        writeln!(logs_file)?;
    }
    writeln!(logs_file)?;
    writeln!(logs_file, "[host network log]")?;
    write!(logs_file, "{network_log}")?;
    if !network_log.ends_with('\n') {
        writeln!(logs_file)?;
    }
    Ok(())
}

fn run_command(program: &str, args: &[String]) -> Result<(), ExecutionError> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(ExecutionError::CommandFailed {
        program: program.to_string(),
        args: args.to_vec(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_command_allowing_codes(
    program: &str,
    args: &[String],
    allowed: &[i32],
) -> Result<(), ExecutionError> {
    let output = Command::new(program).args(args).output()?;
    let status_code = output.status.code().unwrap_or(-1);
    if output.status.success() || allowed.contains(&status_code) {
        return Ok(());
    }

    Err(ExecutionError::CommandFailed {
        program: program.to_string(),
        args: args.to_vec(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}
