use std::{ffi::OsStr, fs, path::Path, process::Command as StdCommand};

use assert_cmd::Command;
use tempfile::TempDir;

fn write_config(root: &Path) -> String {
    let config_path = root.join("config.toml");
    let db_path = root.join("openoman.sqlite");
    let workspace_path = root.join("workspaces");
    let sandbox_runtime_path = root.join("sandbox-runtime");
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nruntime_dir = \"{}\"\n",
            db_path.display().to_string().replace('\\', "\\\\"),
            workspace_path.display().to_string().replace('\\', "\\\\"),
            sandbox_runtime_path.display().to_string().replace('\\', "\\\\"),
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
fn submit_then_run_prepares_git_workspaces() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    init_fixture_repo(&fixture_repo);
    let config = write_config(temp.path());

    let mut submit = cli_cmd();
    let submit_output = submit
        .args([
            "--config",
            &config,
            "submit",
            "--repo",
            &fixture_repo.display().to_string(),
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

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let sandbox_dir = temp
        .path()
        .join("workspaces")
        .join(&job_id)
        .join("sandbox-workspace");
    let trusted_dir = temp
        .path()
        .join("workspaces")
        .join(&job_id)
        .join("trusted-clone");

    assert_eq!(
        fs::read_to_string(sandbox_dir.join("README.md")).expect("sandbox readme"),
        "main branch content\n"
    );
    assert!(!sandbox_dir.join(".git").exists());
    assert!(trusted_dir.join(".git").exists());

    let mut artifacts = cli_cmd();
    let artifacts_output = artifacts
        .args(["--config", &config, "artifacts", &job_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let artifacts_stdout = String::from_utf8(artifacts_output).expect("utf8 artifacts output");

    assert!(artifacts_stdout.contains("workspace.trusted_clone"));
    assert!(artifacts_stdout.contains("workspace.sandbox"));
    assert!(artifacts_stdout.contains("sandbox.patch"));
    assert!(artifacts_stdout.contains("sandbox.report"));
    assert!(artifacts_stdout.contains("sandbox.logs"));
    assert!(artifacts_stdout.contains(&trusted_dir.display().to_string()));
    assert!(artifacts_stdout.contains(&sandbox_dir.display().to_string()));
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

fn init_fixture_repo(path: &Path) {
    fs::create_dir_all(path).expect("create fixture repo");
    git(path, ["init", "-b", "main"]);
    git(path, ["config", "user.name", "Open OMan"]);
    git(path, ["config", "user.email", "openoman@example.com"]);

    fs::write(path.join("README.md"), "initial content\n").expect("write readme");
    git(path, ["add", "README.md"]);
    git(path, ["commit", "-m", "initial"]);

    fs::write(path.join("README.md"), "main branch content\n").expect("write main update");
    git(path, ["add", "README.md"]);
    git(path, ["commit", "-m", "main update"]);
}

fn git(path: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) {
    let status = StdCommand::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .status()
        .expect("git status");
    assert!(status.success(), "git command failed");
}
