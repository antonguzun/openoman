use std::{fs, path::Path, process::Command as StdCommand};

use assert_cmd::Command;
use tempfile::TempDir;

fn write_config(root: &Path) -> String {
    let config_path = root.join("config.toml");
    let db_path = root.join("openoman.sqlite");
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n",
            db_path.display().to_string().replace('\\', "\\\\")
        ),
    )
    .expect("write config");
    config_path.display().to_string()
}

fn cli_cmd() -> Command {
    Command::from_std(StdCommand::new(env!("CARGO_BIN_EXE_openoman")))
}

#[test]
fn submit_then_status_reports_queued_state() {
    let temp = TempDir::new().expect("tempdir");
    let config = write_config(temp.path());

    let mut submit = cli_cmd();
    let submit_output = submit
        .args([
            "--config",
            &config,
            "submit",
            "--repo",
            "github.com/acme/repo",
            "--revision",
            "main",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let submit_stdout = String::from_utf8(submit_output).expect("utf8 output");
    let job_id = submit_stdout
        .trim()
        .strip_prefix("job_id=")
        .expect("job id output")
        .to_string();

    let mut status = cli_cmd();
    status
        .args(["--config", &config, "status", &job_id])
        .assert()
        .success()
        .stdout(format!("job_id={job_id}\nstate=queued\nattempts=0\n"));
}

#[test]
fn missing_config_path_is_deterministic() {
    let mut cmd = cli_cmd();
    cmd.args([
        "--config",
        "./does-not-exist.toml",
        "submit",
        "--repo",
        "r",
        "--revision",
        "main",
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains(
        "failed to read config file ./does-not-exist.toml",
    ));
}
