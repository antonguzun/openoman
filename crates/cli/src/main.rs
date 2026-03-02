use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

use clap::{Parser, Subcommand};
use openoman_core::{
    domain::{
        job::{ArtifactRef, Job, JobId, JobState, RepoRef, Revision},
        plugin::{CheckProfile, PublishPolicy},
    },
    git::{write_canonical_patch, GitAdapter, PreparedWorkspace},
    persistence::{NewArtifactRecord, NewOutboxEvent, OutboxStatus, SqliteStore},
    sandbox::{
        build_sandbox_backend, AgentExecutionSpec, AgentProvider, AttemptSpec,
        CollectedSandboxOutput, FirecrackerBackendConfig, FirecrackerMode, ResourceLimits,
        SandboxBackendKind, SandboxRuntimeConfig, UserPackageDir,
    },
};
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(name = "openoman")]
#[command(about = "OpenOMAN MVP CLI")]
struct Cli {
    /// Path to config file.
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Submit {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        instruction: String,
        #[arg(long, default_value = "unit")]
        check_profile: String,
        #[arg(long, default_value = "on_validation_success")]
        publish_policy: String,
    },
    Run {
        job_id: String,
    },
    Status {
        job_id: String,
    },
    Logs {
        job_id: String,
    },
    Artifacts {
        job_id: String,
    },
    Result {
        job_id: String,
    },
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    core: Option<CoreConfig>,
    git: Option<GitConfig>,
    sandbox: Option<SandboxConfig>,
    agent: Option<AgentConfig>,
}

#[derive(Debug, Deserialize)]
struct CoreConfig {
    database_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitConfig {
    trusted_workspace_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SandboxConfig {
    backend: Option<String>,
    runtime_dir: Option<String>,
    timeout_seconds: Option<u64>,
    memory_mb: Option<u32>,
    cpu_cores: Option<u8>,
    firecracker: Option<FirecrackerConfig>,
}

#[derive(Debug, Deserialize)]
struct FirecrackerConfig {
    mode: Option<String>,
    firecracker_bin: Option<String>,
    jailer_bin: Option<String>,
    kernel_image_path: Option<String>,
    rootfs_image_path: Option<String>,
    guest_cid_base: Option<u32>,
    user_package_dirs: Option<Vec<UserPackageDirConfig>>,
}

#[derive(Debug, Deserialize)]
struct UserPackageDirConfig {
    host_path: String,
    guest_path: String,
    add_to_path: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AgentConfig {
    provider: Option<String>,
    codex_bin: Option<String>,
    egress_proxy_url: Option<String>,
    egress_allowed_domains: Option<Vec<String>>,
}

#[derive(Debug)]
struct AppConfig {
    database_path: PathBuf,
    trusted_workspace_dir: PathBuf,
    sandbox: SandboxRuntimeConfig,
    agent: AgentRuntimeConfig,
}

#[derive(Debug)]
struct AgentRuntimeConfig {
    provider: AgentProvider,
    codex_bin: String,
    egress_proxy: Option<String>,
    egress_allowed_domains: Vec<String>,
}

const LOG_LIMIT_BYTES: usize = 1024 * 1024;
const REPORT_LIMIT_BYTES: usize = 256 * 1024;
const PATCH_LIMIT_BYTES: u64 = 5 * 1024 * 1024;
const TRUNCATION_MARKER: &str = "\n...[truncated by openoman]\n";

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let config = AppConfig::load(&cli.config)?;
    let sandbox_backend = build_sandbox_backend(config.sandbox.clone())
        .map_err(|e| format!("failed to configure sandbox backend: {e}"))?;
    sandbox_backend
        .check_runtime_dependencies()
        .map_err(|e| format!("sandbox backend validation failed: {e}"))?;
    let store = SqliteStore::open(&config.database_path).map_err(|e| {
        format!(
            "failed to open sqlite store at {}: {e}",
            config.database_path.display()
        )
    })?;
    let jobs = store.jobs();

    match cli.command {
        Commands::Submit {
            repo,
            revision,
            instruction,
            check_profile,
            publish_policy,
        } => {
            let id = JobId::new(generate_job_id()).map_err(|e| e.to_string())?;
            let repo_ref = RepoRef::new(repo).map_err(|e| e.to_string())?;
            let revision = Revision::new(revision).map_err(|e| e.to_string())?;
            let check_profile = CheckProfile::new(check_profile).map_err(|e| e.to_string())?;
            let publish_policy =
                PublishPolicy::parse(&publish_policy).map_err(|e| e.to_string())?;

            let (job, event) = Job::submit(
                id,
                repo_ref,
                revision,
                instruction,
                check_profile,
                publish_policy,
            );
            jobs.create(&job).map_err(|e| e.to_string())?;
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
        }
        Commands::Run { job_id } => {
            let job_id = JobId::new(job_id).map_err(|e| e.to_string())?;
            let Some(mut job) = jobs.load(&job_id).map_err(|e| e.to_string())? else {
                return Err(format!("job not found: {}", job_id.as_str()));
            };

            if job.state != JobState::Queued {
                return Err(format!(
                    "job {} is not runnable from state {}",
                    job.id.as_str(),
                    job.state.as_str()
                ));
            }

            let git = GitAdapter::new(&config.trusted_workspace_dir);
            let prepared = git
                .prepare_workspace(&job.repo_ref, &job.revision, job.id.as_str())
                .map_err(|e| {
                    format!(
                        "failed to prepare git workspace for {} at revision {}: {e}",
                        job.repo_ref.as_str(),
                        job.revision.as_str()
                    )
                })?;
            let artifact_records = build_workspace_artifact_records(job.id.as_str(), &prepared)?;
            let mut all_artifact_records = artifact_records;
            let attempt_id = 1;

            let mut runner = sandbox_backend
                .create_runner()
                .map_err(|e| format!("failed to create sandbox runner: {e}"))?;
            let attempt_spec = AttemptSpec {
                job_id: job.id.as_str().to_string(),
                attempt_id,
                workspace_dir: prepared.sandbox_workspace_dir.clone(),
                instruction: job.instruction.clone(),
                limits: config.sandbox.limits.clone(),
                agent: AgentExecutionSpec {
                    provider: config.agent.provider,
                    codex_bin: config.agent.codex_bin.clone(),
                    egress_proxy: config.agent.egress_proxy.clone(),
                    egress_allowed_domains: config.agent.egress_allowed_domains.clone(),
                },
            };

            let handle = runner
                .start(attempt_spec)
                .map_err(|e| format!("failed to start sandbox attempt: {e}"))?;

            job.start_attempt(attempt_id).map_err(|e| e.to_string())?;

            let wait_result = runner
                .wait(&handle)
                .map_err(|e| format!("failed while waiting for sandbox attempt: {e}"))?;

            let collected = runner
                .collect_output(&handle, job.id.as_str(), attempt_id)
                .map_err(|e| format!("failed to collect sandbox output: {e}"));
            let stop_result = runner
                .stop(&handle)
                .map_err(|e| format!("failed to stop sandbox attempt: {e}"));

            let mut failure_reason = if wait_result.timed_out {
                Some(format!(
                    "sandbox attempt timed out for job {}",
                    job.id.as_str()
                ))
            } else if !wait_result.success {
                Some(format!(
                    "sandbox attempt failed for job {} with exit code {:?}",
                    job.id.as_str(),
                    wait_result.code
                ))
            } else {
                None
            };

            if let Ok(collected) = &collected {
                match build_workspace_result_artifact_records(job.id.as_str(), collected) {
                    Ok(records) => all_artifact_records.extend(records),
                    Err(err) if failure_reason.is_none() => {
                        failure_reason = Some(format!(
                            "failed to fingerprint sandbox workspace result: {err}"
                        ));
                    }
                    Err(_) => {}
                }

                if let Err(err) = prepare_text_artifact(&collected.logs_path, LOG_LIMIT_BYTES) {
                    if failure_reason.is_none() {
                        failure_reason = Some(format!("failed to prepare sandbox logs: {err}"));
                    }
                }
                if let Err(err) = prepare_text_artifact(&collected.report_path, REPORT_LIMIT_BYTES)
                {
                    if failure_reason.is_none() {
                        failure_reason = Some(format!("failed to prepare sandbox report: {err}"));
                    }
                }

                match build_file_artifact_records(
                    job.id.as_str(),
                    &[
                        ("sandbox.report", collected.report_path.as_path()),
                        ("sandbox.logs", collected.logs_path.as_path()),
                    ],
                ) {
                    Ok(records) => all_artifact_records.extend(records),
                    Err(err) if failure_reason.is_none() => {
                        failure_reason =
                            Some(format!("failed to store sandbox text artifacts: {err}"));
                    }
                    Err(_) => {}
                }

                let patch_path = collected
                    .report_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("patch.diff");
                let patch_result = write_canonical_patch(
                    &prepared.trusted_clone_dir,
                    &collected.modified_workspace_dir,
                    &patch_path,
                )
                .map_err(|e| format!("failed to generate canonical patch: {e}"))
                .and_then(|_| ensure_patch_size(&patch_path))
                .and_then(|_| {
                    build_file_artifact_record(job.id.as_str(), "sandbox.patch", &patch_path)
                });

                match patch_result {
                    Ok(record) => all_artifact_records.push(record),
                    Err(err) if failure_reason.is_none() => failure_reason = Some(err),
                    Err(_) => {}
                }
            } else if failure_reason.is_none() {
                failure_reason = collected.as_ref().err().cloned();
            }

            if let Err(err) = stop_result {
                return Err(err);
            }

            let artifact_refs = all_artifact_records
                .iter()
                .map(|artifact| ArtifactRef::new(artifact.artifact_ref.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            if !artifact_refs.is_empty() {
                job.collect_artifacts(artifact_refs)
                    .map_err(|e| e.to_string())?;
            }

            if let Some(reason) = failure_reason.clone() {
                if let Ok(collected) = &collected {
                    print_sandbox_logs_to_stderr(&collected.logs_path);
                }
                job.mark_failed(reason.clone()).map_err(|e| e.to_string())?;
            } else {
                job.start_validation().map_err(|e| e.to_string())?;
                job.mark_validation_succeeded().map_err(|e| e.to_string())?;
                job.mark_pull_request_created("https://example.invalid/pr/1")
                    .map_err(|e| e.to_string())?;
                job.mark_succeeded().map_err(|e| e.to_string())?;
            }
            store
                .update_job_and_insert_artifacts(&job, &all_artifact_records)
                .map_err(|e| e.to_string())?;

            if let Some(reason) = failure_reason {
                return Err(reason);
            }

            println!(
                "job {} finished with state={}",
                job.id.as_str(),
                job.state.as_str()
            );
        }
        Commands::Status { job_id } => {
            let job_id = JobId::new(job_id).map_err(|e| e.to_string())?;
            let Some(job) = jobs.load(&job_id).map_err(|e| e.to_string())? else {
                return Err(format!("job not found: {}", job_id.as_str()));
            };

            println!("job_id={}", job.id.as_str());
            println!("state={}", job.state.as_str());
            println!("attempts={}", job.attempts.len());
        }
        Commands::Logs { job_id } => {
            let artifacts = store
                .artifacts()
                .list_by_job(&job_id)
                .map_err(|e| e.to_string())?;
            if let Some(log_artifact) = artifacts
                .iter()
                .find(|artifact| artifact.artifact_ref == "sandbox.logs")
            {
                let contents = fs::read_to_string(&log_artifact.path).map_err(|e| {
                    format!("failed to read sandbox logs {}: {e}", log_artifact.path)
                })?;
                print!("{contents}");
            } else {
                let events = store
                    .outbox()
                    .list_by_job(&job_id)
                    .map_err(|e| e.to_string())?;
                if events.is_empty() {
                    println!("no logs for {job_id}");
                } else {
                    for event in events {
                        println!(
                            "{} {} {}",
                            event.event_id,
                            event.event_type,
                            event.status.as_str()
                        );
                    }
                }
            }
        }
        Commands::Artifacts { job_id } => {
            let artifacts = store
                .artifacts()
                .list_by_job(&job_id)
                .map_err(|e| e.to_string())?;
            if artifacts.is_empty() {
                println!("no artifacts for {job_id}");
            } else {
                for artifact in artifacts {
                    println!(
                        "{} {} {} {}",
                        artifact.artifact_ref, artifact.kind, artifact.path, artifact.size_bytes
                    );
                }
            }
        }
        Commands::Result { job_id } => {
            let job_id = JobId::new(job_id).map_err(|e| e.to_string())?;
            let Some(job) = jobs.load(&job_id).map_err(|e| e.to_string())? else {
                return Err(format!("job not found: {}", job_id.as_str()));
            };

            let result = match job.state {
                JobState::Succeeded => "success",
                JobState::Failed => "failed",
                JobState::Canceled => "canceled",
                _ => "in_progress",
            };
            println!("job_id={} result={}", job.id.as_str(), result);
        }
    }

    Ok(())
}

impl AppConfig {
    fn load(path: &PathBuf) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("failed to read config file {}: {e}", path.display()))?;
        let parsed: FileConfig = toml::from_str(&raw)
            .map_err(|e| format!("invalid config file {}: {e}", path.display()))?;

        let file_db = parsed.core.and_then(|c| c.database_path);
        let env_db = env::var("OPENOMAN_DATABASE_PATH").ok();
        let database_path = env_db.or(file_db).ok_or_else(|| {
            "missing database path; set core.database_path in config file or OPENOMAN_DATABASE_PATH"
                .to_string()
        })?;
        let sandbox = parsed.sandbox.unwrap_or(SandboxConfig {
            backend: None,
            runtime_dir: None,
            timeout_seconds: None,
            memory_mb: None,
            cpu_cores: None,
            firecracker: None,
        });
        let trusted_workspace_dir = parsed
            .git
            .and_then(|g| g.trusted_workspace_dir)
            .unwrap_or_else(|| "./workspaces/trusted".to_string());
        let agent = parsed.agent.unwrap_or(AgentConfig {
            provider: None,
            codex_bin: None,
            egress_proxy_url: None,
            egress_allowed_domains: None,
        });
        let provider = match agent
            .provider
            .unwrap_or_else(|| "codex".to_string())
            .to_lowercase()
            .as_str()
        {
            "codex" => AgentProvider::Codex,
            other => {
                return Err(format!(
                    "unsupported agent provider '{}'; supported providers: codex",
                    other
                ))
            }
        };
        let sandbox_backend = match sandbox
            .backend
            .unwrap_or_else(|| "firecracker".to_string())
            .to_lowercase()
            .as_str()
        {
            "firecracker" => SandboxBackendKind::Firecracker,
            other => {
                return Err(format!(
                    "unsupported sandbox backend '{}'; supported backends: firecracker",
                    other
                ))
            }
        };
        let firecracker = load_firecracker_config(
            sandbox.firecracker,
            path,
            matches!(sandbox_backend, SandboxBackendKind::Firecracker),
        )?;

        Ok(Self {
            database_path: resolve_config_path(path, &database_path)?,
            trusted_workspace_dir: resolve_config_path(path, &trusted_workspace_dir)?,
            sandbox: SandboxRuntimeConfig {
                backend: sandbox_backend,
                runtime_dir: resolve_config_path(
                    path,
                    sandbox
                        .runtime_dir
                        .as_deref()
                        .unwrap_or("./workspaces/sandboxes"),
                )?,
                limits: ResourceLimits {
                    vcpu_count: sandbox.cpu_cores.unwrap_or(2),
                    memory_mib: sandbox.memory_mb.unwrap_or(2048),
                    disk_quota_bytes: 2 * 1024 * 1024 * 1024,
                    timeout_secs: sandbox.timeout_seconds.unwrap_or(1800),
                },
                firecracker,
            },
            agent: AgentRuntimeConfig {
                provider,
                codex_bin: agent.codex_bin.unwrap_or_else(|| "codex".to_string()),
                egress_proxy: agent.egress_proxy_url,
                egress_allowed_domains: normalize_egress_allowed_domains(
                    agent.egress_allowed_domains,
                )?,
            },
        })
    }
}

fn normalize_egress_allowed_domains(domains: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for raw_domain in domains.unwrap_or_default() {
        let domain = raw_domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Err(
                "agent.egress_allowed_domains must not contain empty domain entries".to_string(),
            );
        }
        if !normalized.iter().any(|existing| existing == &domain) {
            normalized.push(domain);
        }
    }
    Ok(normalized)
}

fn load_firecracker_config(
    config: Option<FirecrackerConfig>,
    config_path: &Path,
    enabled: bool,
) -> Result<Option<FirecrackerBackendConfig>, String> {
    if !enabled {
        return Ok(None);
    }

    let firecracker = config.unwrap_or(FirecrackerConfig {
        mode: None,
        firecracker_bin: None,
        jailer_bin: None,
        kernel_image_path: None,
        rootfs_image_path: None,
        guest_cid_base: None,
        user_package_dirs: None,
    });
    let mode = match firecracker
        .mode
        .unwrap_or_else(|| "direct".to_string())
        .to_lowercase()
        .as_str()
    {
        "direct" => FirecrackerMode::Direct,
        "jailer" => FirecrackerMode::Jailer,
        other => {
            return Err(format!(
                "unsupported sandbox.firecracker.mode '{}'; supported modes: direct, jailer",
                other
            ))
        }
    };

    let mut user_package_dirs = Vec::new();
    for package_dir in firecracker.user_package_dirs.unwrap_or_default() {
        user_package_dirs.push(UserPackageDir {
            host_path: resolve_config_path(config_path, &package_dir.host_path)?,
            guest_path: PathBuf::from(package_dir.guest_path),
            add_to_path: package_dir.add_to_path.unwrap_or(false),
        });
    }

    Ok(Some(FirecrackerBackendConfig {
        mode,
        firecracker_bin: firecracker
            .firecracker_bin
            .unwrap_or_else(|| "firecracker".to_string()),
        jailer_bin: firecracker
            .jailer_bin
            .unwrap_or_else(|| "jailer".to_string()),
        kernel_image_path: resolve_config_path(
            config_path,
            firecracker
                .kernel_image_path
                .as_deref()
                .unwrap_or("/opt/openoman/guest/vmlinux"),
        )?,
        rootfs_image_path: resolve_config_path(
            config_path,
            firecracker
                .rootfs_image_path
                .as_deref()
                .unwrap_or("/opt/openoman/guest/rootfs.ext4"),
        )?,
        guest_cid_base: firecracker.guest_cid_base.unwrap_or(10_000),
        user_package_dirs,
    }))
}

fn resolve_config_path(config_path: &Path, raw: &str) -> Result<PathBuf, String> {
    let expanded = expand_home(raw)?;
    if expanded.is_absolute() {
        return Ok(expanded);
    }

    let base_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok(base_dir.join(expanded))
}

fn expand_home(raw: &str) -> Result<PathBuf, String> {
    if raw == "~" {
        let home = env::var("HOME")
            .map_err(|_| "cannot expand '~' because HOME is not set".to_string())?;
        return Ok(PathBuf::from(home));
    }

    if let Some(suffix) = raw.strip_prefix("~/") {
        let home = env::var("HOME")
            .map_err(|_| "cannot expand '~/' because HOME is not set".to_string())?;
        return Ok(PathBuf::from(home).join(suffix));
    }

    Ok(PathBuf::from(raw))
}

fn generate_job_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("job-{now}")
}

fn build_workspace_artifact_records(
    job_id: &str,
    prepared: &PreparedWorkspace,
) -> Result<Vec<NewArtifactRecord>, String> {
    Ok(vec![
        build_workspace_artifact_record(
            job_id,
            "workspace.trusted_clone",
            &prepared.trusted_clone_dir,
        )?,
        build_workspace_artifact_record(
            job_id,
            "workspace.sandbox_input",
            &prepared.sandbox_workspace_dir,
        )?,
    ])
}

fn build_workspace_result_artifact_records(
    job_id: &str,
    collected: &CollectedSandboxOutput,
) -> Result<Vec<NewArtifactRecord>, String> {
    Ok(vec![build_workspace_artifact_record(
        job_id,
        "workspace.sandbox_result",
        &collected.modified_workspace_dir,
    )?])
}

fn build_workspace_artifact_record(
    job_id: &str,
    artifact_ref: &str,
    path: &Path,
) -> Result<NewArtifactRecord, String> {
    let fingerprint = fingerprint_tree(path)?;

    Ok(NewArtifactRecord {
        job_id: job_id.to_string(),
        artifact_ref: artifact_ref.to_string(),
        kind: artifact_ref.to_string(),
        path: path.display().to_string(),
        content_hash: fingerprint.content_hash,
        size_bytes: fingerprint.size_bytes,
    })
}

fn build_file_artifact_records(
    job_id: &str,
    inputs: &[(&str, &Path)],
) -> Result<Vec<NewArtifactRecord>, String> {
    inputs
        .iter()
        .map(|(artifact_ref, path)| build_file_artifact_record(job_id, artifact_ref, path))
        .collect()
}

fn build_file_artifact_record(
    job_id: &str,
    artifact_ref: &str,
    path: &Path,
) -> Result<NewArtifactRecord, String> {
    let contents = fs::read(path)
        .map_err(|e| format!("failed to read artifact file {}: {e}", path.display()))?;
    let mut hasher = Fnv1a::new();
    hasher.update(&contents);

    Ok(NewArtifactRecord {
        job_id: job_id.to_string(),
        artifact_ref: artifact_ref.to_string(),
        kind: artifact_ref.to_string(),
        path: path.display().to_string(),
        content_hash: hasher.finish_hex(),
        size_bytes: i64::try_from(contents.len())
            .map_err(|_| format!("artifact file too large: {}", path.display()))?,
    })
}

fn prepare_text_artifact(path: &Path, limit_bytes: usize) -> Result<(), String> {
    let contents = fs::read(path)
        .map_err(|e| format!("failed to read artifact file {}: {e}", path.display()))?;
    if contents.len() <= limit_bytes {
        return Ok(());
    }

    let marker = TRUNCATION_MARKER.as_bytes();
    let keep_len = limit_bytes.saturating_sub(marker.len());
    let mut truncated = contents[..keep_len].to_vec();
    truncated.extend_from_slice(marker);
    fs::write(path, truncated)
        .map_err(|e| format!("failed to write truncated artifact {}: {e}", path.display()))
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

fn ensure_patch_size(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|e| format!("failed to stat patch artifact {}: {e}", path.display()))?;
    if metadata.len() > PATCH_LIMIT_BYTES {
        return Err(format!(
            "canonical patch exceeds {} bytes at {}",
            PATCH_LIMIT_BYTES,
            path.display()
        ));
    }
    Ok(())
}

struct TreeFingerprint {
    content_hash: String,
    size_bytes: i64,
}

fn fingerprint_tree(path: &Path) -> Result<TreeFingerprint, String> {
    let mut hasher = Fnv1a::new();
    let size_bytes = fingerprint_entry(path, path, &mut hasher)?;
    Ok(TreeFingerprint {
        content_hash: hasher.finish_hex(),
        size_bytes,
    })
}

fn fingerprint_entry(root: &Path, path: &Path, hasher: &mut Fnv1a) -> Result<i64, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to read metadata for {}: {e}", path.display()))?;
    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative_display = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.display().to_string()
    };

    hasher.update(b"path:");
    hasher.update(relative_display.as_bytes());
    hasher.update(b"\n");

    if metadata.is_dir() {
        hasher.update(b"type:dir\n");
        let mut entries = fs::read_dir(path)
            .map_err(|e| format!("failed to list directory {}: {e}", path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("failed to read directory {}: {e}", path.display()))?;
        entries.sort_by_key(|entry| entry.file_name());

        let mut total_size = 0_i64;
        for entry in entries {
            total_size += fingerprint_entry(root, &entry.path(), hasher)?;
        }
        return Ok(total_size);
    }

    if metadata.is_file() {
        hasher.update(b"type:file\n");
        let contents =
            fs::read(path).map_err(|e| format!("failed to read file {}: {e}", path.display()))?;
        hasher.update(&contents);

        return i64::try_from(metadata.len())
            .map_err(|_| format!("file too large to track for artifact {}", path.display()));
    }

    hasher.update(b"type:other\n");
    Ok(0)
}

struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish_hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn app_config_loads_egress_allowed_domains() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[sandbox]
backend = "firecracker"
runtime_dir = "./workspaces/sandboxes"

[sandbox.firecracker]
mode = "direct"
firecracker_bin = "/bin/true"
jailer_bin = "/bin/true"
kernel_image_path = "./guest/out/vmlinux"
rootfs_image_path = "./guest/out/rootfs.ext4"

[agent]
provider = "codex"
codex_bin = "/usr/local/bin/codex"
egress_proxy_url = "http://proxy.internal:3128"
egress_allowed_domains = ["api.openai.com", " api.openai.com ", "files.openai.com"]
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");

        assert_eq!(
            loaded.agent.egress_proxy.as_deref(),
            Some("http://proxy.internal:3128")
        );
        assert_eq!(
            loaded.agent.egress_allowed_domains,
            vec!["api.openai.com".to_string(), "files.openai.com".to_string()]
        );
    }

    #[test]
    fn normalize_egress_allowed_domains_rejects_empty_entries() {
        let err = normalize_egress_allowed_domains(Some(vec![
            "api.openai.com".to_string(),
            "   ".to_string(),
        ]))
        .expect_err("empty entry should fail");

        assert!(err.contains("agent.egress_allowed_domains"));
    }
}
