use std::{
    env, fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

use openoman_core::{
    domain::job::Revision,
    execution::{
        AgentExecutionSpec, AgentProvider, ExecutionBackendConfig, ExecutionBackendKind,
        ExecutionRuntimeConfig, FirecrackerBackendConfig, FirecrackerMode,
        FirecrackerNetworkPrivilegeMode, FirecrackerNetworkingConfig, FirecrackerNetworkingMode,
        HostRiskPosture, ResourceLimits, UserPackageDir,
    },
    github::GitHubPublisherConfig,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct FileConfig {
    core: Option<CoreConfig>,
    git: Option<GitConfig>,
    sandbox: Option<SandboxConfig>,
    agent: Option<AgentConfig>,
    publishing: Option<PublishingConfig>,
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
    host_risk_posture: Option<String>,
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
    network: Option<FirecrackerNetworkConfig>,
}

#[derive(Debug, Deserialize)]
struct FirecrackerNetworkConfig {
    mode: Option<String>,
    privilege_mode: Option<String>,
    tap_name_prefix: Option<String>,
    proxy_port: Option<u16>,
    subnet_cidr: Option<String>,
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
    bin: Option<String>,
    model: Option<String>,
    auth_file: Option<String>,
    api_key: Option<String>,
    api_key_env: Option<String>,
    codex_bin: Option<String>,
    codex_auth_file: Option<String>,
    egress_proxy_url: Option<String>,
    egress_allowed_domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct PublishingConfig {
    provider: Option<String>,
    repo_owner: Option<String>,
    repo_name: Option<String>,
    base_branch: Option<String>,
    branch_prefix: Option<String>,
    api_base_url: Option<String>,
    push_url: Option<String>,
    github_token: Option<String>,
    github_token_env: Option<String>,
    curl_bin: Option<String>,
}

#[derive(Debug)]
pub(crate) struct AppConfig {
    pub(crate) database_path: PathBuf,
    pub(crate) trusted_workspace_dir: PathBuf,
    pub(crate) execution: ExecutionRuntimeConfig,
    pub(crate) agent: AgentRuntimeConfig,
    pub(crate) publishing: Option<PublishingRuntimeConfig>,
}

pub(crate) struct AgentRuntimeConfig {
    provider: AgentProvider,
    bin: String,
    model: Option<String>,
    auth_file: Option<PathBuf>,
    api_key: Option<String>,
    api_key_env: Option<String>,
    egress_proxy: Option<String>,
    egress_allowed_domains: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PublishingRuntimeConfig {
    repo_owner: Option<String>,
    repo_name: Option<String>,
    base_branch: Option<String>,
    branch_prefix: String,
    api_base_url: String,
    push_url: Option<String>,
    token: Option<String>,
    curl_bin: String,
}

#[derive(Debug, Deserialize)]
struct CursorAuthCache {
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
}

#[derive(Debug)]
struct ResolvedCursorExecutionInputs {
    api_key: String,
    staged_auth_file: Option<PathBuf>,
}

enum AgentEgressPolicy<'a> {
    Restricted(&'a [String]),
    AllowAllDebug,
}

impl AppConfig {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("failed to read config file {}: {e}", path.display()))?;
        let parsed: FileConfig = toml::from_str(&raw)
            .map_err(|e| format!("invalid config file {}: {e}", path.display()))?;

        let FileConfig {
            core,
            git,
            sandbox,
            agent,
            publishing,
        } = parsed;
        let file_db = core.and_then(|c| c.database_path);
        let env_db = env::var("OPENOMAN_DATABASE_PATH").ok();
        let database_path = env_db.or(file_db).ok_or_else(|| {
            "missing database path; set core.database_path in config file or OPENOMAN_DATABASE_PATH"
                .to_string()
        })?;
        let sandbox = sandbox.unwrap_or(SandboxConfig {
            backend: None,
            runtime_dir: None,
            timeout_seconds: None,
            memory_mb: None,
            cpu_cores: None,
            host_risk_posture: None,
            firecracker: None,
        });
        let trusted_workspace_dir = git
            .and_then(|g| g.trusted_workspace_dir)
            .unwrap_or_else(|| "./workspaces/trusted".to_string());
        let agent = agent.unwrap_or(AgentConfig {
            provider: None,
            bin: None,
            model: None,
            auth_file: None,
            api_key: None,
            api_key_env: None,
            codex_bin: None,
            codex_auth_file: None,
            egress_proxy_url: None,
            egress_allowed_domains: None,
        });
        let sandbox_backend = match sandbox
            .backend
            .unwrap_or_else(|| "firecracker".to_string())
            .to_lowercase()
            .as_str()
        {
            "firecracker" => ExecutionBackendKind::Firecracker,
            "process" => ExecutionBackendKind::Process,
            other => {
                return Err(format!(
                    "unsupported sandbox backend '{}'; supported backends: firecracker, process",
                    other
                ))
            }
        };
        let host_risk_posture =
            resolve_host_risk_posture(sandbox.host_risk_posture.as_deref(), sandbox_backend)?;
        let firecracker = load_firecracker_config(
            sandbox.firecracker,
            path,
            matches!(sandbox_backend, ExecutionBackendKind::Firecracker),
        )?;
        let agent_runtime = load_agent_runtime_config(agent, path)?;
        validate_agent_networking_contract(firecracker.as_ref(), &agent_runtime)?;

        Ok(Self {
            database_path: resolve_config_path(path, &database_path)?,
            trusted_workspace_dir: resolve_config_path(path, &trusted_workspace_dir)?,
            execution: ExecutionRuntimeConfig {
                backend: match sandbox_backend {
                    ExecutionBackendKind::Firecracker => {
                        let firecracker = firecracker.ok_or_else(|| {
                            "sandbox.firecracker settings are required for backend = \"firecracker\""
                                .to_string()
                        })?;
                        ExecutionBackendConfig::Firecracker(firecracker)
                    }
                    ExecutionBackendKind::Process => ExecutionBackendConfig::Process,
                },
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
                host_risk_posture,
            },
            agent: agent_runtime,
            publishing: load_publishing_config(publishing, path)?,
        })
    }
}

fn resolve_host_risk_posture(
    raw: Option<&str>,
    backend: ExecutionBackendKind,
) -> Result<HostRiskPosture, String> {
    let parsed = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => None,
        Some(value) => Some(match value.to_ascii_lowercase().as_str() {
            "isolated_vm" | "isolated-vm" => HostRiskPosture::IsolatedVm,
            "already_isolated" | "already-isolated" => HostRiskPosture::AlreadyIsolated,
            other => {
                return Err(format!(
                    "unsupported sandbox.host_risk_posture '{}'; supported values: isolated_vm, already_isolated",
                    other
                ))
            }
        }),
    };

    match backend {
        ExecutionBackendKind::Firecracker => Ok(parsed.unwrap_or(HostRiskPosture::IsolatedVm)),
        ExecutionBackendKind::Process => match parsed {
            Some(HostRiskPosture::AlreadyIsolated) => Ok(HostRiskPosture::AlreadyIsolated),
            Some(HostRiskPosture::IsolatedVm) => Err(
                "sandbox.backend = \"process\" requires sandbox.host_risk_posture = \"already_isolated\""
                    .to_string(),
            ),
            None => Err(
                "sandbox.backend = \"process\" requires sandbox.host_risk_posture = \"already_isolated\""
                    .to_string(),
            ),
        },
    }
}

fn load_agent_runtime_config(
    config: AgentConfig,
    config_path: &Path,
) -> Result<AgentRuntimeConfig, String> {
    let provider = parse_agent_provider(config.provider.as_deref())?;
    let bin = resolve_agent_bin(provider, config.bin.as_deref(), config.codex_bin.as_deref())?;
    let model = resolve_agent_model(provider, config.model.as_deref())?;
    let auth_file = resolve_agent_auth_file(
        provider,
        config_path,
        config.auth_file.as_deref(),
        config.codex_auth_file.as_deref(),
    )?;
    let (api_key, api_key_env) = resolve_agent_api_key_config(
        provider,
        config.api_key.as_deref(),
        config.api_key_env.as_deref(),
    )?;

    Ok(AgentRuntimeConfig {
        provider,
        bin,
        model,
        auth_file,
        api_key,
        api_key_env,
        egress_proxy: config.egress_proxy_url,
        egress_allowed_domains: normalize_egress_allowed_domains(config.egress_allowed_domains)?,
    })
}

fn parse_agent_provider(raw: Option<&str>) -> Result<AgentProvider, String> {
    match raw.unwrap_or("codex").trim().to_ascii_lowercase().as_str() {
        "codex" => Ok(AgentProvider::Codex),
        "cursor" => Ok(AgentProvider::Cursor),
        other => Err(format!(
            "unsupported agent provider '{}'; supported providers: codex, cursor",
            other
        )),
    }
}

fn resolve_agent_bin(
    provider: AgentProvider,
    neutral_bin: Option<&str>,
    legacy_codex_bin: Option<&str>,
) -> Result<String, String> {
    let neutral_bin = normalize_optional_string(neutral_bin, "agent.bin")?;
    let legacy_codex_bin = normalize_optional_string(legacy_codex_bin, "agent.codex_bin")?;

    match provider {
        AgentProvider::Codex => match (neutral_bin, legacy_codex_bin) {
            (Some(bin), Some(legacy_bin)) if bin != legacy_bin => Err(
                "agent.bin conflicts with legacy agent.codex_bin; set only one value or make them identical"
                    .to_string(),
            ),
            (Some(bin), _) => Ok(bin),
            (None, Some(bin)) => Ok(bin),
            (None, None) => Ok("codex".to_string()),
        },
        AgentProvider::Cursor => {
            if legacy_codex_bin.is_some() {
                return Err(
                    "agent.codex_bin is a Codex-only compatibility field and cannot be set when agent.provider = \"cursor\""
                        .to_string(),
                );
            }
            Ok(neutral_bin.unwrap_or_else(|| "cursor-agent".to_string()))
        }
    }
}

fn resolve_agent_model(
    provider: AgentProvider,
    model: Option<&str>,
) -> Result<Option<String>, String> {
    let model = normalize_optional_string(model, "agent.model")?;

    match provider {
        AgentProvider::Codex => {
            if model.is_some() {
                return Err(
                    "agent.model is currently supported only when agent.provider = \"cursor\""
                        .to_string(),
                );
            }
            Ok(None)
        }
        AgentProvider::Cursor => Ok(model),
    }
}

fn resolve_agent_auth_file(
    provider: AgentProvider,
    config_path: &Path,
    neutral_auth_file: Option<&str>,
    legacy_codex_auth_file: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let neutral_auth_file =
        resolve_optional_config_path(config_path, neutral_auth_file, "agent.auth_file")?;
    let legacy_codex_auth_file =
        resolve_optional_config_path(config_path, legacy_codex_auth_file, "agent.codex_auth_file")?;

    match provider {
        AgentProvider::Codex => match (neutral_auth_file, legacy_codex_auth_file) {
            (Some(auth_file), Some(legacy_auth_file)) if auth_file != legacy_auth_file => Err(
                "agent.auth_file conflicts with legacy agent.codex_auth_file; set only one value or make them identical"
                    .to_string(),
            ),
            (Some(auth_file), _) => Ok(Some(auth_file)),
            (None, Some(auth_file)) => Ok(Some(auth_file)),
            (None, None) => Ok(None),
        },
        AgentProvider::Cursor => {
            if neutral_auth_file.is_some() || legacy_codex_auth_file.is_some() {
                return Err(
                    "agent.auth_file and legacy agent.codex_auth_file are Codex-only fields and cannot be set when agent.provider = \"cursor\""
                        .to_string(),
                );
            }
            Ok(None)
        }
    }
}

fn resolve_agent_api_key_config(
    provider: AgentProvider,
    api_key: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    let api_key = normalize_optional_string(api_key, "agent.api_key")?;
    let api_key_env = normalize_optional_string(api_key_env, "agent.api_key_env")?;
    match provider {
        AgentProvider::Codex => {
            if api_key.is_some() || api_key_env.is_some() {
                return Err(
                    "agent.api_key and agent.api_key_env are Cursor-only fields and cannot be set when agent.provider = \"codex\""
                        .to_string(),
                );
            }
            Ok((None, None))
        }
        AgentProvider::Cursor => match (api_key, api_key_env) {
            (Some(_), Some(_)) => Err(
                "agent.api_key and agent.api_key_env are mutually exclusive; set only one when agent.provider = \"cursor\""
                    .to_string(),
            ),
            (Some(api_key), None) => Ok((Some(api_key), None)),
            (None, Some(api_key_env)) => Ok((None, Some(api_key_env))),
            (None, None) => Err(
                "agent.api_key or agent.api_key_env is required when agent.provider = \"cursor\""
                    .to_string(),
            ),
        },
    }
}

fn normalize_optional_string(
    raw: Option<&str>,
    field_name: &str,
) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let normalized = raw.trim();
    if normalized.is_empty() {
        return Err(format!("{field_name} must not be empty"));
    }
    Ok(Some(normalized.to_string()))
}

fn resolve_optional_config_path(
    config_path: &Path,
    raw: Option<&str>,
    field_name: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(raw) = normalize_optional_string(raw, field_name)? else {
        return Ok(None);
    };
    resolve_config_path(config_path, &raw).map(Some)
}

impl PublishingRuntimeConfig {
    pub(crate) fn github_config_for_job(
        &self,
        revision: &Revision,
    ) -> Result<GitHubPublisherConfig, String> {
        let repo_owner = self
            .repo_owner
            .clone()
            .ok_or_else(|| "publishing.repo_owner is required for GitHub publishing".to_string())?;
        let repo_name = self
            .repo_name
            .clone()
            .ok_or_else(|| "publishing.repo_name is required for GitHub publishing".to_string())?;
        let token = self
            .token
            .clone()
            .ok_or_else(|| "publishing.github_token or publishing.github_token_env is required for GitHub publishing".to_string())?;
        let base_branch = self
            .base_branch
            .clone()
            .unwrap_or_else(|| revision.as_str().to_string());
        let push_url = match &self.push_url {
            Some(push_url) => push_url.clone(),
            None => format!("https://github.com/{repo_owner}/{repo_name}.git"),
        };

        Ok(GitHubPublisherConfig {
            api_base_url: self.api_base_url.clone(),
            repo_owner,
            repo_name,
            base_branch,
            branch_prefix: self.branch_prefix.clone(),
            push_url,
            token,
            curl_bin: self.curl_bin.clone(),
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

fn agent_egress_policy(domains: &[String]) -> AgentEgressPolicy<'_> {
    if domains.iter().any(|domain| domain == "*") {
        AgentEgressPolicy::AllowAllDebug
    } else {
        AgentEgressPolicy::Restricted(domains)
    }
}

pub(crate) fn resolve_agent_execution_spec(
    config: &AgentRuntimeConfig,
) -> Result<AgentExecutionSpec, String> {
    let (api_key, auth_file) = match config.provider {
        AgentProvider::Codex => (None, config.auth_file.clone()),
        AgentProvider::Cursor => {
            let resolved = resolve_cursor_execution_inputs(config)?;
            (Some(resolved.api_key), resolved.staged_auth_file)
        }
    };

    Ok(AgentExecutionSpec {
        provider: config.provider,
        bin: config.bin.clone(),
        model: config.model.clone(),
        auth_file,
        api_key,
        egress_proxy: config.egress_proxy.clone(),
        egress_allowed_domains: config.egress_allowed_domains.clone(),
    })
}

fn resolve_cursor_execution_inputs(
    config: &AgentRuntimeConfig,
) -> Result<ResolvedCursorExecutionInputs, String> {
    let api_key = if let Some(api_key) = &config.api_key {
        api_key.clone()
    } else {
        let env_name = config.api_key_env.as_deref().ok_or_else(|| {
            "agent.api_key or agent.api_key_env is required when agent.provider = \"cursor\""
                .to_string()
        })?;
        let value = env::var(env_name).map_err(|_| {
            format!("agent.api_key_env references missing environment variable {env_name}")
        })?;
        if value.trim().is_empty() {
            return Err(format!(
                "environment variable {env_name} referenced by agent.api_key_env must not be empty"
            ));
        }
        value
    };

    let staged_auth_file = discover_matching_cursor_auth_cache_path(&api_key);

    Ok(ResolvedCursorExecutionInputs {
        api_key,
        staged_auth_file,
    })
}

fn discover_matching_cursor_auth_cache_path(api_key: &str) -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    discover_matching_cursor_auth_cache_path_in_home(api_key, &PathBuf::from(home))
}

fn discover_matching_cursor_auth_cache_path_in_home(api_key: &str, home: &Path) -> Option<PathBuf> {
    let auth_path = home.join(".config/cursor/auth.json");
    let contents = fs::read_to_string(&auth_path).ok()?;
    let auth_cache: CursorAuthCache = serde_json::from_str(&contents).ok()?;
    if auth_cache.api_key.as_deref() == Some(api_key) {
        Some(auth_path)
    } else {
        None
    }
}

impl std::fmt::Debug for AgentRuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntimeConfig")
            .field("provider", &self.provider)
            .field("bin", &self.bin)
            .field("model", &self.model)
            .field("auth_file", &self.auth_file)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_key_env", &self.api_key_env)
            .field("egress_proxy", &self.egress_proxy)
            .field("egress_allowed_domains", &self.egress_allowed_domains)
            .finish()
    }
}

fn load_publishing_config(
    config: Option<PublishingConfig>,
    config_path: &Path,
) -> Result<Option<PublishingRuntimeConfig>, String> {
    let Some(publishing) = config else {
        return Ok(None);
    };

    let provider = publishing
        .provider
        .unwrap_or_else(|| "github".to_string())
        .to_ascii_lowercase();
    if provider != "github" {
        return Err(format!(
            "unsupported publishing provider '{}'; supported providers: github",
            provider
        ));
    }

    let token_from_env = resolve_github_token(
        publishing.github_token_env.as_deref(),
        publishing.github_token.clone(),
    );
    let push_url = match publishing.push_url {
        Some(push_url) => Some(resolve_push_url(config_path, &push_url)?),
        None => None,
    };

    Ok(Some(PublishingRuntimeConfig {
        repo_owner: publishing.repo_owner,
        repo_name: publishing.repo_name,
        base_branch: publishing.base_branch,
        branch_prefix: publishing
            .branch_prefix
            .unwrap_or_else(|| "openoman".to_string()),
        api_base_url: publishing
            .api_base_url
            .unwrap_or_else(|| "https://api.github.com".to_string()),
        push_url,
        token: token_from_env,
        curl_bin: publishing.curl_bin.unwrap_or_else(|| "curl".to_string()),
    }))
}

fn resolve_github_token(
    github_token_env: Option<&str>,
    github_token: Option<String>,
) -> Option<String> {
    if let Some(token_env) = github_token_env {
        if let Ok(token) = env::var(token_env) {
            return Some(token);
        }
        if looks_like_github_token(token_env) {
            return Some(token_env.to_string());
        }
    } else if let Ok(token) = env::var("OPENOMAN_GITHUB_TOKEN") {
        return Some(token);
    }

    github_token.filter(|token| !token.trim().is_empty())
}

fn looks_like_github_token(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("ghp_")
        || trimmed.starts_with("github_pat_")
        || trimmed.starts_with("gho_")
        || trimmed.starts_with("ghu_")
        || trimmed.starts_with("ghs_")
        || trimmed.starts_with("ghr_")
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
        network: None,
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
    let networking = load_firecracker_networking_config(firecracker.network)?;

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
        networking,
    }))
}

fn load_firecracker_networking_config(
    config: Option<FirecrackerNetworkConfig>,
) -> Result<FirecrackerNetworkingConfig, String> {
    let network = config.unwrap_or(FirecrackerNetworkConfig {
        mode: None,
        privilege_mode: None,
        tap_name_prefix: None,
        proxy_port: None,
        subnet_cidr: None,
    });
    let mode = match network
        .mode
        .unwrap_or_else(|| "disabled".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "disabled" => FirecrackerNetworkingMode::Disabled,
        "host-proxy" => FirecrackerNetworkingMode::HostProxy,
        other => {
            return Err(format!(
                "unsupported sandbox.firecracker.network.mode '{}'; supported modes: disabled, host-proxy",
                other
            ))
        }
    };
    let privilege_mode = match network
        .privilege_mode
        .unwrap_or_else(|| "sudo".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "sudo" => FirecrackerNetworkPrivilegeMode::Sudo,
        "direct" => FirecrackerNetworkPrivilegeMode::Direct,
        other => {
            return Err(format!(
                "unsupported sandbox.firecracker.network.privilege_mode '{}'; supported modes: sudo, direct",
                other
            ))
        }
    };
    let tap_name_prefix = network
        .tap_name_prefix
        .unwrap_or_else(|| "oomtap".to_string());
    validate_tap_name_prefix(&tap_name_prefix)?;
    let proxy_port = network.proxy_port.unwrap_or(3128);
    if proxy_port == 0 {
        return Err("sandbox.firecracker.network.proxy_port must be non-zero".to_string());
    }
    let subnet_cidr = network
        .subnet_cidr
        .unwrap_or_else(|| "172.22.0.0/16".to_string());
    validate_ipv4_cidr(&subnet_cidr, "sandbox.firecracker.network.subnet_cidr")?;

    Ok(FirecrackerNetworkingConfig {
        mode,
        privilege_mode,
        tap_name_prefix,
        proxy_port,
        subnet_cidr,
    })
}

fn validate_agent_networking_contract(
    firecracker: Option<&FirecrackerBackendConfig>,
    agent: &AgentRuntimeConfig,
) -> Result<(), String> {
    const CURSOR_REQUIRED_HOST_PROXY_DOMAIN: &str = "api2.cursor.sh";

    if let Some(auth_file) = &agent.auth_file {
        let metadata = fs::metadata(auth_file).map_err(|e| {
            format!(
                "agent.auth_file does not exist or is not readable at {}: {e}",
                auth_file.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "agent.auth_file must point to a regular file: {}",
                auth_file.display()
            ));
        }
    }

    let Some(firecracker) = firecracker else {
        return Ok(());
    };
    if firecracker.networking.mode != FirecrackerNetworkingMode::HostProxy {
        return Ok(());
    }

    if agent.egress_allowed_domains.is_empty() {
        return Err(
            "agent.egress_allowed_domains must include at least one domain when sandbox.firecracker.network.mode = \"host-proxy\""
                .to_string(),
        );
    }
    if agent.egress_proxy.is_some() {
        return Err(
            "agent.egress_proxy_url must not be set when sandbox.firecracker.network.mode = \"host-proxy\" because the host proxy is configured automatically"
                .to_string(),
        );
    }
    if agent.provider == AgentProvider::Cursor {
        match agent_egress_policy(&agent.egress_allowed_domains) {
            AgentEgressPolicy::AllowAllDebug => {}
            AgentEgressPolicy::Restricted(domains)
                if domains
                    .iter()
                    .any(|domain| domain == CURSOR_REQUIRED_HOST_PROXY_DOMAIN) => {}
            AgentEgressPolicy::Restricted(_) => {
                return Err(format!(
                    "agent.egress_allowed_domains must include \"{CURSOR_REQUIRED_HOST_PROXY_DOMAIN}\" when agent.provider = \"cursor\" and sandbox.firecracker.network.mode = \"host-proxy\" because Cursor CLI print mode tunnels through that hostname"
                ));
            }
        }
    }
    Ok(())
}

fn validate_tap_name_prefix(prefix: &str) -> Result<(), String> {
    if prefix.is_empty() {
        return Err("sandbox.firecracker.network.tap_name_prefix must not be empty".to_string());
    }
    if prefix.len() >= 15 {
        return Err(
            "sandbox.firecracker.network.tap_name_prefix must leave room for a numeric suffix"
                .to_string(),
        );
    }
    if !prefix
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "sandbox.firecracker.network.tap_name_prefix may contain only ASCII letters, digits, '.', '-', and '_'"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_ipv4_cidr(raw: &str, field_name: &str) -> Result<(), String> {
    let (address, prefix) = raw
        .split_once('/')
        .ok_or_else(|| format!("{field_name} must be an IPv4 CIDR like 172.22.0.0/16"))?;
    address
        .parse::<Ipv4Addr>()
        .map_err(|_| format!("{field_name} must contain a valid IPv4 address"))?;
    let prefix_len = prefix
        .parse::<u8>()
        .map_err(|_| format!("{field_name} must contain a numeric prefix length"))?;
    if prefix_len > 30 {
        return Err(format!(
            "{field_name} must have prefix length 30 or smaller so openoman can allocate per-run /30 networks"
        ));
    }
    Ok(())
}

pub(crate) fn ensure_network_privileges(config: &ExecutionRuntimeConfig) -> Result<(), String> {
    let Some(firecracker) = config.firecracker() else {
        return Ok(());
    };
    if firecracker.networking.mode != FirecrackerNetworkingMode::HostProxy {
        return Ok(());
    }
    if firecracker.networking.privilege_mode != FirecrackerNetworkPrivilegeMode::Sudo {
        return Ok(());
    }

    let status = ProcessCommand::new("sudo")
        .arg("-v")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to launch sudo -v for sandbox networking: {e}"))?;
    if !status.success() {
        return Err(
            "failed to acquire sudo credentials for sandbox.firecracker.network.mode = \"host-proxy\""
                .to_string(),
        );
    }
    Ok(())
}

fn resolve_push_url(config_path: &Path, raw: &str) -> Result<String, String> {
    if raw.contains("://") || raw.starts_with("git@") {
        return Ok(raw.to_string());
    }

    Ok(resolve_config_path(config_path, raw)?.display().to_string())
}

fn resolve_config_path(config_path: &Path, raw: &str) -> Result<PathBuf, String> {
    let expanded = expand_home(raw)?;
    if expanded.is_absolute() {
        return Ok(expanded);
    }

    let base_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let absolute_base_dir = if base_dir.is_absolute() {
        base_dir.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|e| format!("failed to resolve current directory for config paths: {e}"))?
            .join(base_dir)
    };
    Ok(absolute_base_dir.join(expanded))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn app_config_loads_egress_allowed_domains() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        let auth_path = temp.path().join("auth.json");
        fs::write(&auth_path, "{}").expect("write auth file");
        fs::write(
            &config_path,
            format!(
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
bin = "/usr/local/bin/codex"
auth_file = "{}"
egress_proxy_url = "http://proxy.internal:3128"
egress_allowed_domains = ["api.openai.com", " api.openai.com ", "files.openai.com"]
"#,
                auth_path.display()
            ),
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");

        assert_eq!(loaded.agent.bin, "/usr/local/bin/codex");
        assert_eq!(loaded.agent.auth_file.as_deref(), Some(auth_path.as_path()));
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
    fn app_config_loads_firecracker_host_proxy_networking() {
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

[sandbox.firecracker.network]
mode = "host-proxy"
privilege_mode = "direct"
tap_name_prefix = "oomtap"
proxy_port = 4128
subnet_cidr = "172.30.0.0/16"

[agent]
provider = "codex"
bin = "/usr/local/bin/codex"
egress_allowed_domains = ["api.openai.com"]
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        let firecracker = loaded.execution.firecracker().expect("firecracker config");
        assert_eq!(
            firecracker.networking.mode,
            FirecrackerNetworkingMode::HostProxy
        );
        assert_eq!(
            firecracker.networking.privilege_mode,
            FirecrackerNetworkPrivilegeMode::Direct
        );
        assert_eq!(firecracker.networking.tap_name_prefix, "oomtap");
        assert_eq!(firecracker.networking.proxy_port, 4128);
        assert_eq!(firecracker.networking.subnet_cidr, "172.30.0.0/16");
    }

    #[test]
    fn app_config_loads_process_backend_with_explicit_risk_posture() {
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
backend = "process"
runtime_dir = "./workspaces/sandboxes"
host_risk_posture = "already_isolated"

[agent]
provider = "codex"
bin = "/usr/local/bin/codex"
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        assert_eq!(
            loaded.execution.backend_kind(),
            ExecutionBackendKind::Process
        );
        assert_eq!(
            loaded.execution.host_risk_posture,
            HostRiskPosture::AlreadyIsolated
        );
    }

    #[test]
    fn app_config_rejects_process_backend_without_explicit_risk_posture() {
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
backend = "process"
runtime_dir = "./workspaces/sandboxes"

[agent]
provider = "codex"
bin = "/usr/local/bin/codex"
"#,
        )
        .expect("write config");

        let err =
            AppConfig::load(&config_path).expect_err("process backend should require risk posture");
        assert!(err.contains("sandbox.host_risk_posture"));
        assert!(err.contains("already_isolated"));
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

    #[test]
    fn normalize_egress_allowed_domains_preserves_wildcard() {
        let domains =
            normalize_egress_allowed_domains(Some(vec!["*".to_string(), " * ".to_string()]))
                .expect("wildcard should be accepted");

        assert_eq!(domains, vec!["*".to_string()]);
    }

    #[test]
    fn host_proxy_mode_rejects_manual_proxy_url() {
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

[sandbox.firecracker.network]
mode = "host-proxy"

[agent]
provider = "codex"
bin = "/usr/local/bin/codex"
egress_proxy_url = "http://proxy.internal:3128"
egress_allowed_domains = ["api.openai.com"]
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("manual proxy should be rejected");
        assert!(err.contains("agent.egress_proxy_url"));
        assert!(err.contains("host-proxy"));
    }

    #[test]
    fn app_config_loads_legacy_codex_aliases() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        let auth_path = temp.path().join("auth.json");
        fs::write(&auth_path, "{}").expect("write auth file");
        fs::write(
            &config_path,
            format!(
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
codex_auth_file = "{}"
"#,
                auth_path.display()
            ),
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        assert_eq!(loaded.agent.bin, "/usr/local/bin/codex");
        assert_eq!(loaded.agent.model, None);
        assert_eq!(loaded.agent.auth_file.as_deref(), Some(auth_path.as_path()));
        assert_eq!(loaded.agent.api_key_env, None);
    }

    #[test]
    fn app_config_loads_cursor_provider() {
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
provider = "cursor"
bin = "/usr/local/bin/cursor-agent"
model = "gpt-5"
api_key = "cursor-test-key"
egress_allowed_domains = ["api2.cursor.sh"]
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        assert_eq!(loaded.agent.provider, AgentProvider::Cursor);
        assert_eq!(loaded.agent.bin, "/usr/local/bin/cursor-agent");
        assert_eq!(loaded.agent.model.as_deref(), Some("gpt-5"));
        assert_eq!(loaded.agent.api_key.as_deref(), Some("cursor-test-key"));
        assert_eq!(loaded.agent.api_key_env.as_deref(), None);
        assert_eq!(loaded.agent.auth_file, None);
    }

    #[test]
    fn app_config_rejects_conflicting_codex_bin_fields() {
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
bin = "/usr/local/bin/codex"
codex_bin = "/usr/bin/codex"
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("conflicting bin fields should fail");
        assert!(err.contains("agent.bin conflicts"));
    }

    #[test]
    fn app_config_rejects_cursor_without_api_key_or_env() {
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
provider = "cursor"
bin = "/usr/local/bin/cursor-agent"
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("missing api_key_env should fail");
        assert!(err.contains("agent.api_key or agent.api_key_env"));
        assert!(err.contains("cursor"));
    }

    #[test]
    fn app_config_rejects_cursor_with_codex_aliases() {
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
provider = "cursor"
bin = "/usr/local/bin/cursor-agent"
api_key = "cursor-test-key"
codex_bin = "/usr/local/bin/codex"
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("cursor should reject codex aliases");
        assert!(err.contains("agent.codex_bin"));
        assert!(err.contains("cursor"));
    }

    #[test]
    fn app_config_rejects_cursor_with_both_api_key_and_env() {
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
provider = "cursor"
bin = "/usr/local/bin/cursor-agent"
api_key = "cursor-test-key"
api_key_env = "OPENOMAN_CURSOR_API_KEY"
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("ambiguous cursor auth should fail");
        assert!(err.contains("agent.api_key and agent.api_key_env are mutually exclusive"));
    }

    #[test]
    fn app_config_rejects_cursor_host_proxy_without_api2_cursor_sh() {
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

[sandbox.firecracker.network]
mode = "host-proxy"
tap_name_prefix = "oomtap"
subnet_cidr = "172.22.0.0/30"

[agent]
provider = "cursor"
bin = "/usr/local/bin/cursor-agent"
api_key = "cursor-test-key"
egress_allowed_domains = ["api.cursor.com"]
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("cursor host-proxy should require api2");
        assert!(err.contains("api2.cursor.sh"));
        assert!(err.contains("agent.egress_allowed_domains"));
    }

    #[test]
    fn app_config_accepts_cursor_host_proxy_wildcard_allowlist() {
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

[sandbox.firecracker.network]
mode = "host-proxy"
tap_name_prefix = "oomtap"
subnet_cidr = "172.22.0.0/30"

[agent]
provider = "cursor"
bin = "/usr/local/bin/cursor-agent"
api_key = "cursor-test-key"
egress_allowed_domains = ["*"]
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("wildcard allowlist should load");
        assert_eq!(loaded.agent.egress_allowed_domains, vec!["*".to_string()]);
    }

    #[test]
    fn app_config_rejects_missing_agent_auth_file() {
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
bin = "/usr/local/bin/codex"
auth_file = "./missing-auth.json"
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("missing auth file should fail");
        assert!(err.contains("agent.auth_file"));
        assert!(err.contains("missing-auth.json"));
    }

    #[test]
    fn app_config_rejects_codex_model() {
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
bin = "/usr/local/bin/codex"
model = "gpt-5"
"#,
        )
        .expect("write config");

        let err = AppConfig::load(&config_path).expect_err("codex should reject agent.model");
        assert!(err.contains("agent.model"));
        assert!(err.contains("cursor"));
    }

    #[test]
    fn resolve_agent_execution_spec_uses_configured_cursor_api_key() {
        let config = AgentRuntimeConfig {
            provider: AgentProvider::Cursor,
            bin: "cursor-agent".to_string(),
            model: Some("gpt-5".to_string()),
            auth_file: None,
            api_key: Some("cursor-inline-secret".to_string()),
            api_key_env: None,
            egress_proxy: None,
            egress_allowed_domains: vec!["api2.cursor.sh".to_string()],
        };

        let spec = resolve_agent_execution_spec(&config)
            .expect("cursor execution should use inline api key");
        assert_eq!(spec.model.as_deref(), Some("gpt-5"));
        assert_eq!(spec.api_key.as_deref(), Some("cursor-inline-secret"));
    }

    #[test]
    fn discover_matching_cursor_auth_cache_path_in_home_matches_api_key() {
        let temp = TempDir::new().expect("tempdir");
        let auth_dir = temp.path().join(".config/cursor");
        fs::create_dir_all(&auth_dir).expect("create auth dir");
        let auth_path = auth_dir.join("auth.json");
        fs::write(
            &auth_path,
            r#"{"apiKey":"cursor-inline-secret","accessToken":"token","refreshToken":"token"}"#,
        )
        .expect("write auth file");

        let discovered =
            discover_matching_cursor_auth_cache_path_in_home("cursor-inline-secret", temp.path());
        assert_eq!(discovered.as_deref(), Some(auth_path.as_path()));
    }

    #[test]
    fn discover_matching_cursor_auth_cache_path_in_home_ignores_mismatched_api_key() {
        let temp = TempDir::new().expect("tempdir");
        let auth_dir = temp.path().join(".config/cursor");
        fs::create_dir_all(&auth_dir).expect("create auth dir");
        fs::write(
            auth_dir.join("auth.json"),
            r#"{"apiKey":"different-secret","accessToken":"token","refreshToken":"token"}"#,
        )
        .expect("write auth file");

        let discovered =
            discover_matching_cursor_auth_cache_path_in_home("cursor-inline-secret", temp.path());
        assert_eq!(discovered, None);
    }

    #[test]
    fn resolve_agent_execution_spec_requires_cursor_host_env() {
        let config = AgentRuntimeConfig {
            provider: AgentProvider::Cursor,
            bin: "cursor-agent".to_string(),
            model: None,
            auth_file: None,
            api_key: None,
            api_key_env: Some("OPENOMAN_CURSOR_API_KEY_TEST_DOES_NOT_EXIST".to_string()),
            egress_proxy: None,
            egress_allowed_domains: vec!["api2.cursor.sh".to_string()],
        };

        let err = resolve_agent_execution_spec(&config)
            .expect_err("cursor execution should require an API key env var");
        assert!(err.contains("agent.api_key_env"));
        assert!(err.contains("OPENOMAN_CURSOR_API_KEY_TEST_DOES_NOT_EXIST"));
    }

    #[test]
    fn app_config_accepts_raw_github_token_in_github_token_env_field() {
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
bin = "/usr/local/bin/codex"

[publishing]
provider = "github"
repo_owner = "antonguzun"
repo_name = "kickfoss"
github_token_env = "github_pat_example123"
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        let publishing = loaded.publishing.expect("publishing config");
        assert_eq!(publishing.token.as_deref(), Some("github_pat_example123"));
    }
}
