use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use assert_cmd::Command;
use openoman_core::{domain::job::JobId, persistence::SqliteStore};
use tempfile::TempDir;

struct PublishingTestConfig {
    repo_owner: String,
    repo_name: String,
    base_branch: String,
    push_url: String,
    api_base_url: String,
    github_token: String,
    curl_bin: PathBuf,
}

struct AgentTestConfig<'a> {
    provider: &'a str,
    bin: &'a Path,
    model: Option<&'a str>,
    api_key: Option<&'a str>,
    api_key_env: Option<&'a str>,
}

fn codex_agent(bin: &Path) -> AgentTestConfig<'_> {
    AgentTestConfig {
        provider: "codex",
        bin,
        model: None,
        api_key: None,
        api_key_env: None,
    }
}

fn cursor_agent(bin: &Path) -> AgentTestConfig<'_> {
    AgentTestConfig {
        provider: "cursor",
        bin,
        model: None,
        api_key: Some("cursor-test-key"),
        api_key_env: None,
    }
}

fn cursor_agent_from_env(bin: &Path) -> AgentTestConfig<'_> {
    AgentTestConfig {
        provider: "cursor",
        bin,
        model: None,
        api_key: None,
        api_key_env: Some("OPENOMAN_CURSOR_API_KEY"),
    }
}

fn write_config(
    root: &Path,
    firecracker_bin: &Path,
    agent: &AgentTestConfig<'_>,
    publishing: Option<&PublishingTestConfig>,
) -> String {
    let config_path = root.join("config.toml");
    let db_path = root.join("openoman.sqlite");
    let workspace_path = root.join("workspaces");
    let sandbox_runtime_path = root.join("sandbox-runtime");
    let kernel_path = root.join("vmlinux");
    let rootfs_path = root.join("rootfs.ext4");
    let publishing_block = publishing
        .map(|publishing| {
            format!(
                "\n[publishing]\nprovider = \"github\"\nrepo_owner = \"{}\"\nrepo_name = \"{}\"\nbase_branch = \"{}\"\npush_url = \"{}\"\napi_base_url = \"{}\"\ngithub_token = \"{}\"\ncurl_bin = \"{}\"\n",
                publishing.repo_owner,
                publishing.repo_name,
                publishing.base_branch,
                publishing.push_url.replace('\\', "\\\\"),
                publishing.api_base_url,
                publishing.github_token,
                publishing.curl_bin.display().to_string().replace('\\', "\\\\"),
            )
        })
        .unwrap_or_default();
    fs::write(&kernel_path, "kernel").expect("write fake kernel");
    fs::write(&rootfs_path, "rootfs").expect("write fake rootfs");
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nbackend = \"firecracker\"\nruntime_dir = \"{}\"\ntimeout_seconds = 30\nmemory_mb = 512\ncpu_cores = 1\n\n[sandbox.firecracker]\nmode = \"direct\"\nfirecracker_bin = \"{}\"\njailer_bin = \"{}\"\nkernel_image_path = \"{}\"\nrootfs_image_path = \"{}\"\n\n[agent]\nprovider = \"{}\"\nbin = \"{}\"\n{}{}{}{}",
            db_path.display().to_string().replace('\\', "\\\\"),
            workspace_path.display().to_string().replace('\\', "\\\\"),
            sandbox_runtime_path.display().to_string().replace('\\', "\\\\"),
            firecracker_bin.display().to_string().replace('\\', "\\\\"),
            firecracker_bin.display().to_string().replace('\\', "\\\\"),
            kernel_path.display().to_string().replace('\\', "\\\\"),
            rootfs_path.display().to_string().replace('\\', "\\\\"),
            agent.provider,
            agent.bin.display().to_string().replace('\\', "\\\\"),
            agent
                .model
                .map(|model| format!("model = \"{model}\"\n"))
                .unwrap_or_default(),
            agent
                .api_key
                .map(|key| format!("api_key = \"{key}\"\n"))
                .unwrap_or_default(),
            agent
                .api_key_env
                .map(|name| format!("api_key_env = \"{name}\"\n"))
                .unwrap_or_default(),
            publishing_block,
        ),
    )
    .expect("write config");
    config_path.display().to_string()
}

fn write_process_config(
    root: &Path,
    agent: &AgentTestConfig<'_>,
    publishing: Option<&PublishingTestConfig>,
) -> String {
    let config_path = root.join("config.toml");
    let db_path = root.join("openoman.sqlite");
    let workspace_path = root.join("workspaces");
    let sandbox_runtime_path = root.join("sandbox-runtime");
    let publishing_block = publishing
        .map(|publishing| {
            format!(
                "\n[publishing]\nprovider = \"github\"\nrepo_owner = \"{}\"\nrepo_name = \"{}\"\nbase_branch = \"{}\"\npush_url = \"{}\"\napi_base_url = \"{}\"\ngithub_token = \"{}\"\ncurl_bin = \"{}\"\n",
                publishing.repo_owner,
                publishing.repo_name,
                publishing.base_branch,
                publishing.push_url.replace('\\', "\\\\"),
                publishing.api_base_url,
                publishing.github_token,
                publishing.curl_bin.display().to_string().replace('\\', "\\\\"),
            )
        })
        .unwrap_or_default();
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nbackend = \"process\"\nhost_risk_posture = \"already_isolated\"\nruntime_dir = \"{}\"\ntimeout_seconds = 30\nmemory_mb = 512\ncpu_cores = 1\n\n[agent]\nprovider = \"{}\"\nbin = \"{}\"\n{}{}{}{}",
            db_path.display().to_string().replace('\\', "\\\\"),
            workspace_path.display().to_string().replace('\\', "\\\\"),
            sandbox_runtime_path.display().to_string().replace('\\', "\\\\"),
            agent.provider,
            agent.bin.display().to_string().replace('\\', "\\\\"),
            agent
                .model
                .map(|model| format!("model = \"{model}\"\n"))
                .unwrap_or_default(),
            agent
                .api_key
                .map(|key| format!("api_key = \"{key}\"\n"))
                .unwrap_or_default(),
            agent
                .api_key_env
                .map(|name| format!("api_key_env = \"{name}\"\n"))
                .unwrap_or_default(),
            publishing_block,
        ),
    )
    .expect("write config");
    config_path.display().to_string()
}

fn write_relative_config(
    root: &Path,
    firecracker_bin: &Path,
    agent: &AgentTestConfig<'_>,
    publishing: Option<&PublishingTestConfig>,
) -> String {
    let config_path = root.join("config.toml");
    let db_path = root.join("openoman.sqlite");
    let workspace_path = root.join("workspaces");
    let sandbox_runtime_path = root.join("sandbox-runtime");
    let kernel_path = root.join("vmlinux");
    let rootfs_path = root.join("rootfs.ext4");
    let publishing_block = publishing
        .map(|publishing| {
            format!(
                "\n[publishing]\nprovider = \"github\"\nrepo_owner = \"{}\"\nrepo_name = \"{}\"\nbase_branch = \"{}\"\npush_url = \"{}\"\napi_base_url = \"{}\"\ngithub_token = \"{}\"\ncurl_bin = \"{}\"\n",
                publishing.repo_owner,
                publishing.repo_name,
                publishing.base_branch,
                relativize_path(root, Path::new(&publishing.push_url)),
                publishing.api_base_url,
                publishing.github_token,
                relativize_command_path(root, &publishing.curl_bin),
            )
        })
        .unwrap_or_default();
    fs::write(&kernel_path, "kernel").expect("write fake kernel");
    fs::write(&rootfs_path, "rootfs").expect("write fake rootfs");
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\n\n[sandbox]\nbackend = \"firecracker\"\nruntime_dir = \"{}\"\ntimeout_seconds = 30\nmemory_mb = 512\ncpu_cores = 1\n\n[sandbox.firecracker]\nmode = \"direct\"\nfirecracker_bin = \"{}\"\njailer_bin = \"{}\"\nkernel_image_path = \"{}\"\nrootfs_image_path = \"{}\"\n\n[agent]\nprovider = \"{}\"\nbin = \"{}\"\n{}{}{}{}",
            relativize_path(root, &db_path),
            relativize_path(root, &workspace_path),
            relativize_path(root, &sandbox_runtime_path),
            relativize_command_path(root, firecracker_bin),
            relativize_command_path(root, firecracker_bin),
            relativize_path(root, &kernel_path),
            relativize_path(root, &rootfs_path),
            agent.provider,
            relativize_command_path(root, agent.bin),
            agent
                .model
                .map(|model| format!("model = \"{model}\"\n"))
                .unwrap_or_default(),
            agent
                .api_key
                .map(|key| format!("api_key = \"{key}\"\n"))
                .unwrap_or_default(),
            agent
                .api_key_env
                .map(|name| format!("api_key_env = \"{name}\"\n"))
                .unwrap_or_default(),
            publishing_block,
        ),
    )
    .expect("write config");
    "./config.toml".to_string()
}

fn relativize_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("path should live under root")
        .display()
        .to_string()
}

fn relativize_command_path(root: &Path, path: &Path) -> String {
    format!("./{}", relativize_path(root, path))
}

fn cli_cmd() -> Command {
    Command::from_std(StdCommand::new(env!("CARGO_BIN_EXE_openoman")))
}

#[test]
fn submit_then_status_reports_queued_state() {
    let temp = TempDir::new().expect("tempdir");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        None,
    );

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
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        None,
    );

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
fn submit_then_run_persists_canonical_artifacts_with_process_backend() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_codex = write_fake_codex(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_process_config(temp.path(), &codex_agent(&fake_codex), None);

    let job_id = submit_job(&config, &fixture_repo, "add empty line in readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let runtime_attempt_dir = temp
        .path()
        .join("sandbox-runtime")
        .join("jobs")
        .join(&job_id)
        .join("attempt-1");
    let patch_path = runtime_attempt_dir.join("patch.diff");
    let patch_contents = fs::read_to_string(&patch_path).expect("patch file");
    assert!(patch_contents.contains("README.md"));

    let report = fs::read_to_string(runtime_attempt_dir.join("report.txt")).expect("report");
    let logs = fs::read_to_string(runtime_attempt_dir.join("logs.txt")).expect("logs");
    assert!(report.contains("fake codex completed"));
    assert!(logs.contains("openoman process backend started"));
    assert!(logs.contains("fake codex applied instruction"));
}

#[test]
fn submit_then_run_supports_cursor_provider() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_cursor = write_fake_cursor(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &cursor_agent(&fake_cursor),
        None,
    );

    let job_id = submit_job(&config, &fixture_repo, "add empty line in readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let attempt_dir = temp
        .path()
        .join("sandbox-runtime")
        .join("jobs")
        .join(&job_id)
        .join("attempt-1");
    let report = fs::read_to_string(attempt_dir.join("report.txt")).expect("read report");
    let logs = fs::read_to_string(attempt_dir.join("logs.txt")).expect("read logs");
    let patch = fs::read_to_string(attempt_dir.join("patch.diff")).expect("read patch");

    assert!(report.contains("fake cursor completed"));
    assert!(logs.contains("agent provider: cursor"));
    assert!(logs.contains("cursor api key present"));
    assert!(patch.contains("README.md"));
}

#[test]
fn submit_then_run_passes_cursor_model() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_cursor = write_fake_cursor(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &AgentTestConfig {
            provider: "cursor",
            bin: &fake_cursor,
            model: Some("gpt-5"),
            api_key: Some("cursor-test-key"),
            api_key_env: None,
        },
        None,
    );

    let job_id = submit_job(&config, &fixture_repo, "add empty line in readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let attempt_dir = temp
        .path()
        .join("sandbox-runtime")
        .join("jobs")
        .join(&job_id)
        .join("attempt-1");
    let logs = fs::read_to_string(attempt_dir.join("logs.txt")).expect("read logs");

    assert!(logs.contains("agent model: gpt-5"));
}

#[test]
fn cursor_run_requires_configured_api_key_env() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_cursor = write_fake_cursor(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &cursor_agent_from_env(&fake_cursor),
        None,
    );

    let job_id = submit_job(&config, &fixture_repo, "add empty line in readme");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "agent.api_key_env references missing environment variable OPENOMAN_CURSOR_API_KEY",
        ));
}

#[test]
fn logs_print_sandbox_log_contents_when_present() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        None,
    );

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
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        None,
    );

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
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        None,
    );
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

#[test]
fn submit_run_and_result_show_github_publish_metadata() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let publish_remote = temp.path().join("publish-remote.git");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    let fake_curl = write_fake_curl(temp.path());
    init_fixture_repo(&fixture_repo);
    git(
        temp.path(),
        [
            OsStr::new("clone"),
            OsStr::new("--bare"),
            fixture_repo.as_os_str(),
            publish_remote.as_os_str(),
        ],
    );
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        Some(&PublishingTestConfig {
            repo_owner: "acme".to_string(),
            repo_name: "demo".to_string(),
            base_branch: "main".to_string(),
            push_url: publish_remote.display().to_string(),
            api_base_url: "https://api.example.test".to_string(),
            github_token: "test-token".to_string(),
            curl_bin: fake_curl,
        }),
    );

    let job_id = submit_job_with_policy(
        &config,
        &fixture_repo,
        "add empty line in readme",
        "on_validation_success",
    );

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let branch_name = format!("openoman/{job_id}");
    let mut result = cli_cmd();
    let result_output = result
        .args(["--config", &config, "result", &job_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result_stdout = String::from_utf8(result_output).expect("utf8 result output");
    assert!(result_stdout.contains(&format!("job_id={job_id} result=success")));
    assert!(result_stdout.contains(&format!("branch={branch_name}")));
    assert!(result_stdout.contains("pull_request_number=17"));
    assert!(result_stdout.contains("pull_request_url=https://example.test/pulls/17"));

    let remote_branch = git_output(
        temp.path(),
        [
            OsStr::new("--git-dir"),
            publish_remote.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new(&format!("refs/heads/{branch_name}")),
        ],
    );
    assert!(!remote_branch.trim().is_empty());

    let remote_readme = git_output(
        temp.path(),
        [
            OsStr::new("--git-dir"),
            publish_remote.as_os_str(),
            OsStr::new("show"),
            OsStr::new(&format!("refs/heads/{branch_name}:README.md")),
        ],
    );
    assert!(remote_readme.contains("main branch content"));

    let store = SqliteStore::open(temp.path().join("openoman.sqlite")).expect("open sqlite store");
    let outbox_events = store
        .outbox()
        .list_by_job(&job_id)
        .expect("list outbox events");
    let pr_event = outbox_events
        .iter()
        .find(|event| event.event_type == "job.pr_created")
        .expect("pull request event should exist");
    assert!(pr_event.payload.contains(&branch_name));
    assert!(pr_event.payload.contains("https://example.test/pulls/17"));
}

#[test]
fn submit_run_with_relative_config_paths_publishes_successfully() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let publish_remote = temp.path().join("publish-remote.git");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    let fake_curl = write_fake_curl(temp.path());
    init_fixture_repo(&fixture_repo);
    git(
        temp.path(),
        [
            OsStr::new("clone"),
            OsStr::new("--bare"),
            fixture_repo.as_os_str(),
            publish_remote.as_os_str(),
        ],
    );
    let config = write_relative_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        Some(&PublishingTestConfig {
            repo_owner: "acme".to_string(),
            repo_name: "demo".to_string(),
            base_branch: "main".to_string(),
            push_url: publish_remote.display().to_string(),
            api_base_url: "https://api.example.test".to_string(),
            github_token: "test-token".to_string(),
            curl_bin: fake_curl,
        }),
    );

    let job_id = {
        let mut submit = cli_cmd();
        let submit_output = submit
            .current_dir(temp.path())
            .args([
                "--config",
                &config,
                "submit",
                "--repo",
                &fixture_repo.display().to_string(),
                "--revision",
                "main",
                "--instruction",
                "add empty line in readme",
                "--publish-policy",
                "on_validation_success",
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
    };

    let mut run = cli_cmd();
    run.current_dir(temp.path())
        .args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let branch_name = format!("openoman/{job_id}");
    let remote_branch = git_output(
        temp.path(),
        [
            OsStr::new("--git-dir"),
            publish_remote.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new(&format!("refs/heads/{branch_name}")),
        ],
    );
    assert!(!remote_branch.trim().is_empty());

    let store = SqliteStore::open(temp.path().join("openoman.sqlite")).expect("open sqlite store");
    let artifacts = store
        .artifacts()
        .list_by_job(&job_id)
        .expect("list artifact records");
    let patch_artifact = artifacts
        .iter()
        .find(|artifact| artifact.artifact_ref == "sandbox.patch")
        .expect("patch artifact should exist");
    assert!(Path::new(&patch_artifact.path).is_absolute());
}

#[test]
fn publish_policy_never_skips_pull_request_creation() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    let fake_firecracker = write_fake_firecracker(temp.path());
    let fake_codex = write_fake_codex(temp.path());
    init_fixture_repo(&fixture_repo);
    let config = write_config(
        temp.path(),
        &fake_firecracker,
        &codex_agent(&fake_codex),
        None,
    );

    let job_id =
        submit_job_with_policy(&config, &fixture_repo, "add empty line in readme", "never");

    let mut run = cli_cmd();
    run.args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .stdout(format!("job {job_id} finished with state=succeeded\n"));

    let mut result = cli_cmd();
    let result_output = result
        .args(["--config", &config, "result", &job_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result_stdout = String::from_utf8(result_output).expect("utf8 result output");
    assert!(result_stdout.contains(&format!("job_id={job_id} result=success")));
    assert!(!result_stdout.contains("branch="));
    assert!(!result_stdout.contains("pull_request_url="));
}

#[test]
fn submit_with_repo_alias_applies_env_overlay_and_reports_publish_warning() {
    let temp = TempDir::new().expect("tempdir");
    let fixture_repo = temp.path().join("fixture-repo");
    init_fixture_repo(&fixture_repo);
    fs::create_dir_all(temp.path().join("env_for_repo/demo")).expect("create env dir");
    fs::write(
        temp.path().join("env_for_repo/demo/.env"),
        "DEMO_TOKEN=test\n",
    )
    .expect("write env");
    fs::write(
        temp.path().join("env_for_repo/demo/.env.test"),
        "DEMO_MODE=1\n",
    )
    .expect("write env test");
    let fake_codex = write_fake_codex(temp.path());
    let db_path = temp.path().join("openoman.sqlite");
    let config_path = temp.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "[core]\ndatabase_path = \"{}\"\n\n[git]\ntrusted_workspace_dir = \"{}\"\nenv_for_repo_dir = \"{}\"\n\n[[git.accounts]]\nalias = \"demo-account\"\ngit_user_name = \"Repo Bot\"\ngit_user_email = \"repo-bot@example.test\"\n\n[[git.repos]]\nalias = \"demo\"\nrepo_ref = \"{}\"\nplatform = \"gitlab\"\naccount = \"demo-account\"\nenv_repo_name = \"demo\"\n\n[sandbox]\nbackend = \"process\"\nhost_risk_posture = \"already_isolated\"\nruntime_dir = \"{}\"\ntimeout_seconds = 30\nmemory_mb = 512\ncpu_cores = 1\n\n[agent]\nprovider = \"codex\"\nbin = \"{}\"\n",
            db_path.display().to_string().replace('\\', "\\\\"),
            temp.path()
                .join("workspaces")
                .display()
                .to_string()
                .replace('\\', "\\\\"),
            temp.path()
                .join("env_for_repo")
                .display()
                .to_string()
                .replace('\\', "\\\\"),
            fixture_repo.display().to_string().replace('\\', "\\\\"),
            temp.path()
                .join("sandbox-runtime")
                .display()
                .to_string()
                .replace('\\', "\\\\"),
            fake_codex.display().to_string().replace('\\', "\\\\"),
        ),
    )
    .expect("write config");

    let config = config_path.display().to_string();
    let mut submit = cli_cmd();
    let submit_output = submit
        .args([
            "--config",
            &config,
            "submit",
            "--repo",
            "demo",
            "--revision",
            "main",
            "--instruction",
            "add empty line in readme",
            "--publish-policy",
            "on_validation_success",
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
    let run_output = run
        .args(["--config", &config, "run", &job_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let run_stdout = String::from_utf8(run_output).expect("utf8 run output");
    assert!(run_stdout.contains("publish_warning="));
    assert!(run_stdout.contains(&format!("job {job_id} finished with state=succeeded")));

    let mut result = cli_cmd();
    let result_output = result
        .args(["--config", &config, "result", &job_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result_stdout = String::from_utf8(result_output).expect("utf8 result output");
    assert!(result_stdout.contains("publish_warning="));
    assert!(!result_stdout.contains("branch="));
    assert!(!result_stdout.contains("pull_request_url="));

    let store = SqliteStore::open(&db_path).expect("open sqlite");
    let job = store
        .jobs()
        .load(&JobId::new(job_id.clone()).expect("job id"))
        .expect("load job")
        .expect("job exists");
    assert_eq!(job.repo_alias.as_deref(), Some("demo"));
    assert_eq!(job.repo_ref.as_str(), fixture_repo.display().to_string());

    let artifacts = store
        .artifacts()
        .list_by_job(&job_id)
        .expect("load artifacts");
    let workspace = artifacts
        .iter()
        .find(|artifact| artifact.artifact_ref == "workspace.sandbox_result")
        .expect("workspace artifact");
    assert!(Path::new(&workspace.path).join(".env").exists());
    assert!(Path::new(&workspace.path).join(".env.test").exists());

    let patch = artifacts
        .iter()
        .find(|artifact| artifact.artifact_ref == "sandbox.patch")
        .expect("patch artifact");
    let patch_contents = fs::read_to_string(&patch.path).expect("patch contents");
    assert!(!patch_contents.contains(".env"));
}

fn submit_job(config: &str, repo_path: &Path, instruction: &str) -> String {
    submit_job_with_policy(config, repo_path, instruction, "never")
}

fn submit_job_with_policy(
    config: &str,
    repo_path: &Path,
    instruction: &str,
    publish_policy: &str,
) -> String {
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
            "--publish-policy",
            publish_policy,
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
  printf "fake codex failed: %s\n" "$instruction" > "$report"
  echo "fake codex simulated failure"
  exit 9
fi
printf "fake codex completed: %s\n" "$instruction" > "$report"
echo "fake codex applied instruction"
"#,
    )
    .expect("write fake codex");
    fs::set_permissions(&script_path, PermissionsExt::from_mode(0o755)).expect("chmod fake codex");
    script_path
}

fn write_fake_cursor(root: &Path) -> PathBuf {
    let script_path = root.join("fake-cursor-agent.sh");
    fs::write(
        &script_path,
        r#"#!/usr/bin/env sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "cursor-agent 0.0-test"
  exit 0
fi
if [ "${CURSOR_API_KEY:-}" = "" ]; then
  echo "missing CURSOR_API_KEY" >&2
  exit 11
fi
print_mode=0
model=""
explicit_api_key=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --api-key)
      explicit_api_key="$2"
      shift 2
      ;;
    -p|--print|-f|--force)
      shift 1
      print_mode=1
      ;;
    --model)
      model="$2"
      shift 2
      ;;
    --output-format)
      shift 2
      ;;
    *)
      instruction="$1"
      shift 1
      ;;
  esac
done
if [ "$print_mode" -ne 1 ]; then
  echo "expected print mode" >&2
  exit 12
fi
if [ "$explicit_api_key" = "" ]; then
  echo "missing --api-key" >&2
  exit 14
fi
if [ "$explicit_api_key" != "${CURSOR_API_KEY:-}" ]; then
  echo "--api-key mismatch" >&2
  exit 15
fi
if printf "%s" "${instruction:-}" | grep -q "readme"; then
  printf "\n" >> README.md
else
  printf "agent touched workspace\n" > AGENT_OUTPUT.txt
fi
if printf "%s" "${instruction:-}" | grep -q "fail"; then
  echo "fake cursor failed: ${instruction:-}"
  exit 13
fi
echo "fake cursor completed: ${instruction:-}${model:+ (model=$model)}"
"#,
    )
    .expect("write fake cursor");
    fs::set_permissions(&script_path, PermissionsExt::from_mode(0o755)).expect("chmod fake cursor");
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
debugfs -R "dump -p /openoman-config/agent.env $tmpdir/agent.env" "$image" >/dev/null 2>&1
instruction="$(cat "$tmpdir/instruction.txt")"
. "$tmpdir/agent.env"
debugfs -R "dump -p /workspace/README.md $tmpdir/README.md" "$image" >/dev/null 2>&1 || true
if printf "%s" "$instruction" | grep -qi "readme"; then
  printf "\n" >> "$tmpdir/README.md"
  debugfs -w -R "rm /workspace/README.md" "$image" >/dev/null 2>&1 || true
  debugfs -w -R "write $tmpdir/README.md /workspace/README.md" "$image" >/dev/null 2>&1
else
  printf "agent touched workspace\n" > "$tmpdir/AGENT_OUTPUT.txt"
  debugfs -w -R "write $tmpdir/AGENT_OUTPUT.txt /workspace/AGENT_OUTPUT.txt" "$image" >/dev/null 2>&1
fi
printf "agent provider: %s\nagent bin: %s\nagent model: %s\ninstruction: %s\n" "${AGENT_PROVIDER:-unknown}" "${AGENT_BIN:-missing}" "${AGENT_MODEL:-default}" "$instruction" > "$tmpdir/logs.txt"
case "${AGENT_PROVIDER:-unknown}" in
  codex)
    printf "fake codex completed: %s\n" "$instruction" > "$tmpdir/report.txt"
    ;;
  cursor)
    if [ -z "${CURSOR_API_KEY:-}" ]; then
      printf "cursor api key missing\n" >> "$tmpdir/logs.txt"
      printf "fake cursor failed: missing api key for %s\n" "$instruction" > "$tmpdir/report.txt"
      debugfs -w -R "write $tmpdir/logs.txt /openoman-output/logs.txt" "$image" >/dev/null 2>&1
      debugfs -w -R "write $tmpdir/report.txt /openoman-output/report.txt" "$image" >/dev/null 2>&1
      exit 19
    fi
    printf "cursor api key present\n" >> "$tmpdir/logs.txt"
    printf "fake cursor completed: %s\n" "$instruction" > "$tmpdir/report.txt"
    ;;
  *)
    printf "unsupported provider in fake firecracker\n" >> "$tmpdir/logs.txt"
    printf "fake firecracker failed: unsupported provider %s\n" "${AGENT_PROVIDER:-unknown}" > "$tmpdir/report.txt"
    debugfs -w -R "write $tmpdir/logs.txt /openoman-output/logs.txt" "$image" >/dev/null 2>&1
    debugfs -w -R "write $tmpdir/report.txt /openoman-output/report.txt" "$image" >/dev/null 2>&1
    exit 20
    ;;
esac
printf "fake firecracker completed\n" >> "$tmpdir/logs.txt"
debugfs -w -R "write $tmpdir/logs.txt /openoman-output/logs.txt" "$image" >/dev/null 2>&1
debugfs -w -R "write $tmpdir/report.txt /openoman-output/report.txt" "$image" >/dev/null 2>&1
if printf "%s" "$instruction" | grep -qi "fail"; then
  exit 9
fi
exit 0
"#,
    )
    .expect("write fake firecracker");
    fs::set_permissions(&script_path, PermissionsExt::from_mode(0o755))
        .expect("chmod fake firecracker");
    script_path
}

fn write_fake_curl(root: &Path) -> PathBuf {
    let script_path = root.join("fake-curl.sh");
    fs::write(
        &script_path,
        r#"#!/usr/bin/env sh
set -eu
printf '{"number":17,"html_url":"https://example.test/pulls/17"}\n201'
"#,
    )
    .expect("write fake curl");
    fs::set_permissions(&script_path, PermissionsExt::from_mode(0o755)).expect("chmod fake curl");
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

fn git_output(path: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> String {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("git output");
    assert!(output.status.success(), "git command failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
