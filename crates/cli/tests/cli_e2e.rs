use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use assert_cmd::Command;
use tempfile::TempDir;

fn write_config(root: &Path, firecracker_bin: &Path, codex_bin: &Path) -> String {
    let config_path = root.join("config.toml");
    let db_path = root.join("openoman.sqlite");
    let workspace_path = root.join("workspaces");
    let sandbox_runtime_path = root.join("sandbox-runtime");
    let kernel_path = root.join("vmlinux");
    let rootfs_path = root.join("rootfs.ext4");
    fs::write(&kernel_path, "kernel").expect("write fake kernel");
    fs::write(&rootfs_path, "rootfs").expect("write fake rootfs");
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nbackend = \"firecracker\"\nruntime_dir = \"{}\"\ntimeout_seconds = 30\nmemory_mb = 512\ncpu_cores = 1\n\n[sandbox.firecracker]\nmode = \"direct\"\nfirecracker_bin = \"{}\"\njailer_bin = \"{}\"\nkernel_image_path = \"{}\"\nrootfs_image_path = \"{}\"\n\n[agent]\nprovider = \"codex\"\ncodex_bin = \"{}\"\n",
            db_path.display().to_string().replace('\\', "\\\\"),
            workspace_path.display().to_string().replace('\\', "\\\\"),
            sandbox_runtime_path.display().to_string().replace('\\', "\\\\"),
            firecracker_bin.display().to_string().replace('\\', "\\\\"),
            firecracker_bin.display().to_string().replace('\\', "\\\\"),
            kernel_path.display().to_string().replace('\\', "\\\\"),
            rootfs_path.display().to_string().replace('\\', "\\\\"),
            codex_bin.display().to_string().replace('\\', "\\\\"),
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
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    let config = write_config(temp.path(), &fake_firecracker, &fake_codex);

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
            "--instruction",
            "create a patch",
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
fn submit_then_run_persists_canonical_artifacts() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(temp.path(), &fake_firecracker, &fake_codex);

    let job_id = submit_job(&config, &fixture_repo, "add empty line in readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let sandbox_input_dir = temp
        .path()
        .join("workspaces")
        .join(&job_id)
        .join("sandbox-workspace");
    let trusted_dir = temp
        .path()
        .join("workspaces")
        .join(&job_id)
        .join("trusted-clone");
    let runtime_attempt_dir = temp
        .path()
        .join("sandbox-runtime")
        .join("jobs")
        .join(&job_id)
        .join("attempt-1");

    assert!(sandbox_input_dir.join(".git").exists());
    let sandbox_config =
        fs::read_to_string(sandbox_input_dir.join(".git").join("config")).expect("sandbox config");
    assert!(!sandbox_config.contains("[remote \"origin\"]"));
    assert!(trusted_dir.join(".git").exists());

    let patch_path = runtime_attempt_dir.join("patch.diff");
    let patch_contents = fs::read_to_string(&patch_path).expect("patch file");
    assert!(patch_contents.contains("README.md"));
    assert!(!patch_contents.contains("EPIC6_AGENT.txt"));

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
    assert!(artifacts_stdout.contains("workspace.sandbox_input"));
    assert!(artifacts_stdout.contains("workspace.sandbox_result"));
    assert!(artifacts_stdout.contains("sandbox.patch"));
    assert!(artifacts_stdout.contains("sandbox.report"));
    assert!(artifacts_stdout.contains("sandbox.logs"));
    assert!(artifacts_stdout.contains(&trusted_dir.display().to_string()));
    assert!(artifacts_stdout.contains(&sandbox_input_dir.display().to_string()));
    assert!(artifacts_stdout.contains(
        &runtime_attempt_dir
            .join("workspace-result")
            .display()
            .to_string()
    ));
}

#[test]
fn logs_print_sandbox_log_contents_when_present() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(temp.path(), &fake_firecracker, &fake_codex);

    let job_id = submit_job(&config, &fixture_repo, "add empty line in readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success();

    let mut logs = cli_cmd();
    logs.args(["--config", &config, "logs", &job_id])
        .assert()
        .success()
        .stdout(predicates::str::contains("fake firecracker completed"))
        .stdout(predicates::str::contains(
            "instruction: add empty line in readme",
        ));
}

#[test]
fn failing_sandbox_attempt_persists_artifacts_and_marks_job_failed() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(temp.path(), &fake_firecracker, &fake_codex);

    let job_id = submit_job(&config, &fixture_repo, "fail after touching readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("sandbox logs:"))
        .stderr(predicates::str::contains("fake firecracker completed"))
        .stderr(predicates::str::contains("sandbox attempt failed"));

    let mut status = cli_cmd();
    status
        .args(["--config", &config, "status", &job_id])
        .assert()
        .success()
        .stdout(format!("job_id={job_id}\nstate=failed\nattempts=1\n"));

    let attempt_dir = temp
        .path()
        .join("sandbox-runtime")
        .join("jobs")
        .join(&job_id)
        .join("attempt-1");
    let patch_contents =
        fs::read_to_string(attempt_dir.join("patch.diff")).expect("patch exists after failure");
    assert!(patch_contents.contains("README.md"));
    assert!(attempt_dir.join("logs.txt").exists());
    assert!(attempt_dir.join("report.txt").exists());
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
        "--instruction",
        "do work",
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains(
        "failed to read config file ./does-not-exist.toml",
    ));
}

#[test]
fn jailer_mode_fails_fast_with_clear_error() {
    let temp = TempDir::new().expect("tempdir");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    let config = write_config(temp.path(), &fake_firecracker, &fake_codex);
    let config_contents = fs::read_to_string(&config).expect("read config");
    fs::write(
        &config,
        config_contents.replace("mode = \"direct\"", "mode = \"jailer\""),
    )
    .expect("write jailer config");

    let mut cmd = cli_cmd();
    cmd.args([
        "--config",
        &config,
        "submit",
        "--repo",
        "github.com/acme/repo",
        "--revision",
        "main",
        "--instruction",
        "create a patch",
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains(
        "sandbox backend validation failed: not implemented",
    ))
    .stderr(predicates::str::contains("mode = \"direct\""));
}

#[test]
fn internal_firecracker_helper_bypasses_config_loading_and_validates_args() {
    let mut cmd = cli_cmd();
    cmd.args([
        "--config",
        "./does-not-exist.toml",
        "internal",
        "firecracker-net",
        "setup",
        "--tap-name",
        "bad name",
        "--host-ip",
        "172.22.0.1",
        "--prefix-len",
        "30",
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains(
        "tap_name may contain only ASCII letters",
    ));
}

fn submit_job(config: &str, repo_path: &Path, instruction: &str) -> String {
    let mut submit = cli_cmd();
    let submit_output = submit
        .args([
            "--config",
            config,
            "submit",
            "--repo",
            &repo_path.display().to_string(),
            "--revision",
            "main",
            "--instruction",
            instruction,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let submit_stdout = String::from_utf8(submit_output).expect("utf8 output");
    submit_stdout
        .trim()
        .strip_prefix("job_id=")
        .expect("job id output")
        .to_string()
}

fn write_fake_codex(root: &Path) -> PathBuf {
    let script_path = root.join("fake-codex.sh");
    fs::write(
        &script_path,
        r#"#!/usr/bin/env sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "codex-cli 0.0-test"
  exit 0
fi
if [ "${1:-}" != "exec" ]; then
  echo "unexpected invocation" >&2
  exit 2
fi
shift
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
    --full-auto|--color)
      if [ "$1" = "--color" ]; then
        shift 2
      else
        shift 1
      fi
      ;;
    *)
      instruction="$1"
      shift 1
      ;;
  esac
done
if [ -z "$report" ]; then
  echo "missing report path" >&2
  exit 3
fi
cd "$workdir"
if printf "%s" "$instruction" | grep -q "readme"; then
  printf "\n" >> README.md
else
  printf "agent touched workspace\n" > AGENT_OUTPUT.txt
fi
if printf "%s" "$instruction" | grep -q "fail"; then
  printf "fake codex failed: %s\n" "$instruction" > "../$report"
  echo "fake codex simulated failure"
  exit 9
fi
printf "fake codex completed: %s\n" "$instruction" > "../$report"
echo "fake codex applied instruction"
"#,
    )
    .expect("write fake codex");
    fs::set_permissions(
        &script_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod fake codex");
    script_path
}

fn write_fake_firecracker(root: &Path) -> PathBuf {
    let script_path = root.join("fake-firecracker.sh");
    fs::write(
        &script_path,
        r#"#!/usr/bin/env sh
set -eu
image="${OPENOMAN_FAKE_RUNTIME_IMAGE:?}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
debugfs -R "dump -p /openoman-config/instruction.txt $tmpdir/instruction.txt" "$image" >/dev/null 2>&1
instruction="$(cat "$tmpdir/instruction.txt")"
debugfs -R "dump -p /workspace/README.md $tmpdir/README.md" "$image" >/dev/null 2>&1 || true
if printf "%s" "$instruction" | grep -qi "readme"; then
  printf "\n" >> "$tmpdir/README.md"
  debugfs -w -R "rm /workspace/README.md" "$image" >/dev/null 2>&1 || true
  debugfs -w -R "write $tmpdir/README.md /workspace/README.md" "$image" >/dev/null 2>&1
else
  printf "agent touched workspace\n" > "$tmpdir/AGENT_OUTPUT.txt"
  debugfs -w -R "write $tmpdir/AGENT_OUTPUT.txt /workspace/AGENT_OUTPUT.txt" "$image" >/dev/null 2>&1
fi
printf "instruction: %s\nfake firecracker completed\n" "$instruction" > "$tmpdir/logs.txt"
printf "fake firecracker completed: %s\n" "$instruction" > "$tmpdir/report.txt"
debugfs -w -R "write $tmpdir/logs.txt /openoman-output/logs.txt" "$image" >/dev/null 2>&1
debugfs -w -R "write $tmpdir/report.txt /openoman-output/report.txt" "$image" >/dev/null 2>&1
if printf "%s" "$instruction" | grep -qi "fail"; then
  exit 9
fi
exit 0
"#,
    )
    .expect("write fake firecracker");
    fs::set_permissions(
        &script_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod fake firecracker");
    script_path
}

fn init_fixture_repo(path: &Path) {
    fs::create_dir_all(path).expect("create fixture repo");
    git(path, ["init", "-b", "main"]);
    git(path, ["config", "user.name", "Open OMan"]);
    git(path, ["config", "user.email", "openoman@example.com"]);

    fs::write(path.join("README.md"), "initial content\n").expect("write readme");
    fs::write(path.join("notes.txt"), "first note\n").expect("write notes");
    git(path, ["add", "README.md", "notes.txt"]);
    git(path, ["commit", "-m", "initial"]);

    fs::write(path.join("README.md"), "main branch content\n").expect("write main update");
    fs::write(path.join("notes.txt"), "kept note\n").expect("write notes update");
    git(path, ["add", "README.md", "notes.txt"]);
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
