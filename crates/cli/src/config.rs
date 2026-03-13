use std::{
    collections::HashMap,
    env, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

use openoman_core::{
    agents::{
        load_agent_runtime_config as core_load_agent_runtime_config,
        resolve_agent_execution_spec as core_resolve_agent_execution_spec,
        validate_agent_networking_contract as core_validate_agent_networking_contract,
        AgentRuntimeConfig, AgentRuntimeConfigInput,
    },
    domain::job::Revision,
    execution::{
        AgentExecutionSpec, ExecutionBackendConfig, ExecutionBackendKind, ExecutionRuntimeConfig,
        FirecrackerBackendConfig, FirecrackerMode, FirecrackerNetworkPrivilegeMode,
        FirecrackerNetworkingConfig, FirecrackerNetworkingMode, HostRiskPosture, ResourceLimits,
        UserPackageDir,
    },
    github::GitHubPublisherConfig,
    gitlab::GitLabPublisherConfig,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct FileConfig {
    core: Option<CoreConfig>,
    git: Option<GitConfig>,
    sandbox: Option<SandboxConfig>,
    agent: Option<AgentConfig>,
    server: Option<ServerConfig>,
    publishing: Option<PublishingConfig>,
}

#[derive(Debug, Deserialize)]
struct CoreConfig {
    database_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitConfig {
    trusted_workspace_dir: Option<String>,
    env_for_repo_dir: Option<String>,
    accounts: Option<Vec<GitAccountConfig>>,
    repos: Option<Vec<GitRepoConfig>>,
}

#[derive(Debug, Deserialize)]
struct GitAccountConfig {
    alias: String,
    token: Option<String>,
    token_env: Option<String>,
    git_user_name: Option<String>,
    git_user_email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitRepoConfig {
    alias: String,
    repo_ref: String,
    platform: Option<String>,
    account: Option<String>,
    env_repo_name: Option<String>,
    repo_owner: Option<String>,
    repo_name: Option<String>,
    base_branch: Option<String>,
    branch_prefix: Option<String>,
    api_base_url: Option<String>,
    push_url: Option<String>,
    curl_bin: Option<String>,
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

#[derive(Debug, Deserialize)]
struct ServerConfig {
    host: Option<String>,
    port: Option<u16>,
    auth_token: Option<String>,
    auth_token_env: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) database_path: PathBuf,
    pub(crate) trusted_workspace_dir: PathBuf,
    pub(crate) env_for_repo_dir: PathBuf,
    pub(crate) execution: ExecutionRuntimeConfig,
    pub(crate) agent: AgentRuntimeConfig,
    pub(crate) server: ServerRuntimeConfig,
    pub(crate) publishing: Option<PublishingRuntimeConfig>,
    repo_catalog: RepoCatalogRuntimeConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct ServerRuntimeConfig {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) auth_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSubmitRepo {
    pub(crate) repo_ref: String,
    pub(crate) repo_alias: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum PublishRuntimePlan {
    GitHub(GitHubPublisherConfig),
    GitLab(GitLabPublisherConfig),
    SkipWithWarning(String),
}

#[derive(Debug, Clone)]
struct RepoCatalogRuntimeConfig {
    repos_by_alias: HashMap<String, RepoRuntimeConfig>,
    accounts_by_alias: HashMap<String, GitAccountRuntimeConfig>,
}

#[derive(Debug, Clone)]
struct RepoRuntimeConfig {
    repo_ref: String,
    platform: RepoPlatform,
    account_alias: Option<String>,
    env_repo_name: String,
    publish: RepoPublishRuntimeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoPlatform {
    GitHub,
    GitLab,
    GitLabSelfHosted,
}

impl RepoPlatform {
    fn parse(raw: Option<&str>) -> Result<Self, String> {
        let value = raw.unwrap_or("github").trim().to_ascii_lowercase();
        match value.as_str() {
            "github" => Ok(Self::GitHub),
            "gitlab" => Ok(Self::GitLab),
            "gitlab_self_hosted" | "gitlab-self-hosted" | "self_hosted_gitlab"
            | "self-hosted-gitlab" => Ok(Self::GitLabSelfHosted),
            other => Err(format!(
                "unsupported git repo platform '{}'; supported values: github, gitlab, gitlab_self_hosted",
                other
            )),
        }
    }
}

#[derive(Debug, Clone)]
struct RepoPublishRuntimeConfig {
    repo_owner: Option<String>,
    repo_name: Option<String>,
    base_branch: Option<String>,
    branch_prefix: String,
    api_base_url: Option<String>,
    push_url: Option<String>,
    curl_bin: String,
}

#[derive(Debug, Clone)]
struct GitAccountRuntimeConfig {
    token: Option<String>,
    git_user_name: String,
    git_user_email: String,
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
            server,
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
        let git = git.unwrap_or(GitConfig {
            trusted_workspace_dir: None,
            env_for_repo_dir: None,
            accounts: None,
            repos: None,
        });
        let trusted_workspace_dir = git
            .trusted_workspace_dir
            .clone()
            .unwrap_or_else(|| "./workspaces/trusted".to_string());
        let env_for_repo_dir = git
            .env_for_repo_dir
            .clone()
            .unwrap_or_else(|| "./env_for_repo".to_string());
        let repo_catalog = load_repo_catalog_config(&git, path)?;
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
            env_for_repo_dir: resolve_config_path(path, &env_for_repo_dir)?,
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
            server: load_server_config(server)?,
            publishing: load_publishing_config(publishing, path)?,
            repo_catalog,
        })
    }

    pub(crate) fn resolve_submit_repo(
        &self,
        repo_or_alias: &str,
    ) -> Result<ResolvedSubmitRepo, String> {
        let candidate = repo_or_alias.trim();
        if candidate.is_empty() {
            return Err("submit --repo must not be empty".to_string());
        }

        if let Some(repo) = self.repo_catalog.repos_by_alias.get(candidate) {
            return Ok(ResolvedSubmitRepo {
                repo_ref: repo.repo_ref.clone(),
                repo_alias: Some(candidate.to_string()),
            });
        }

        Ok(ResolvedSubmitRepo {
            repo_ref: candidate.to_string(),
            repo_alias: None,
        })
    }

    pub(crate) fn env_overlay_dir_for_alias(&self, repo_alias: Option<&str>) -> Option<PathBuf> {
        let alias = repo_alias?;
        let repo = self.repo_catalog.repos_by_alias.get(alias)?;
        let path = self.env_for_repo_dir.join(&repo.env_repo_name);
        if path.is_dir() {
            Some(path)
        } else {
            None
        }
    }

    pub(crate) fn resolve_clone_token_for_job(&self, repo_alias: Option<&str>) -> Option<String> {
        if let Some(alias) = repo_alias {
            let repo = self.repo_catalog.repos_by_alias.get(alias)?;
            let account_alias = repo.account_alias.as_deref()?;
            let account = self.repo_catalog.accounts_by_alias.get(account_alias)?;
            return account.token.clone();
        }

        self.publishing
            .as_ref()
            .and_then(|legacy| legacy.token.clone())
            .and_then(|token| {
                let trimmed = token.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
    }

    pub(crate) fn resolve_publish_plan_for_job(
        &self,
        repo_alias: Option<&str>,
        revision: &Revision,
    ) -> Result<PublishRuntimePlan, String> {
        if let Some(alias) = repo_alias {
            let Some(repo) = self.repo_catalog.repos_by_alias.get(alias) else {
                return Ok(PublishRuntimePlan::SkipWithWarning(format!(
                    "publishing skipped: repo alias '{}' is not configured in git.repos",
                    alias
                )));
            };

            let Some(account_alias) = repo.account_alias.as_deref() else {
                return Ok(PublishRuntimePlan::SkipWithWarning(format!(
                    "publishing skipped: repo alias '{}' has no bound git account",
                    alias
                )));
            };
            let Some(account) = self.repo_catalog.accounts_by_alias.get(account_alias) else {
                return Ok(PublishRuntimePlan::SkipWithWarning(format!(
                    "publishing skipped: bound git account '{}' was not found",
                    account_alias
                )));
            };
            let Some(token) = account.token.clone() else {
                return Ok(PublishRuntimePlan::SkipWithWarning(format!(
                    "publishing skipped: git account '{}' has no token configured",
                    account_alias
                )));
            };
            let base_branch = repo
                .publish
                .base_branch
                .clone()
                .unwrap_or_else(|| revision.as_str().to_string());

            return match repo.platform {
                RepoPlatform::GitHub => {
                    let inferred_repo = infer_github_repo_identity(
                        repo.publish.push_url.as_deref(),
                        Some(&repo.repo_ref),
                    );
                    let repo_owner = repo
                        .publish
                        .repo_owner
                        .clone()
                        .or_else(|| inferred_repo.as_ref().map(|inferred| inferred.owner.clone()))
                        .ok_or_else(|| {
                            format!(
                                "git.repos alias '{}' is missing repo_owner and it could not be inferred from repo_ref/push_url; set repo_owner explicitly",
                                alias
                            )
                        })?;
                    let repo_name = repo
                        .publish
                        .repo_name
                        .clone()
                        .or_else(|| inferred_repo.as_ref().map(|inferred| inferred.name.clone()))
                        .ok_or_else(|| {
                            format!(
                                "git.repos alias '{}' is missing repo_name and it could not be inferred from repo_ref/push_url; set repo_name explicitly",
                                alias
                            )
                        })?;
                    let push_url = repo.publish.push_url.clone().unwrap_or_else(|| {
                        format!("https://github.com/{repo_owner}/{repo_name}.git")
                    });

                    Ok(PublishRuntimePlan::GitHub(GitHubPublisherConfig {
                        api_base_url: repo
                            .publish
                            .api_base_url
                            .clone()
                            .unwrap_or_else(|| "https://api.github.com".to_string()),
                        repo_owner,
                        repo_name,
                        base_branch,
                        branch_prefix: repo.publish.branch_prefix.clone(),
                        push_url,
                        token,
                        curl_bin: repo.publish.curl_bin.clone(),
                        git_user_name: account.git_user_name.clone(),
                        git_user_email: account.git_user_email.clone(),
                    }))
                }
                RepoPlatform::GitLab | RepoPlatform::GitLabSelfHosted => {
                    let explicit_project_path = repo
                        .publish
                        .repo_owner
                        .as_ref()
                        .zip(repo.publish.repo_name.as_ref())
                        .map(|(owner, name)| {
                            format!("{}/{}", owner.trim_matches('/'), name.trim_matches('/'))
                        });
                    let inferred_project = infer_gitlab_project_identity(
                        repo.publish.push_url.as_deref(),
                        Some(&repo.repo_ref),
                    )
                    .map(|identity| identity.project_path);
                    let project_path = explicit_project_path
                        .or(inferred_project)
                        .ok_or_else(|| {
                            format!(
                                "git.repos alias '{}' is missing repo_owner/repo_name and the GitLab project path could not be inferred from repo_ref/push_url; set repo_owner and repo_name explicitly",
                                alias
                            )
                        })?;
                    let inferred_host = infer_git_remote_location(
                        repo.publish.push_url.as_deref(),
                        Some(&repo.repo_ref),
                    )
                    .map(|location| location.host);
                    let api_base_url = match repo.publish.api_base_url.clone() {
                        Some(value) => value,
                        None => match repo.platform {
                            RepoPlatform::GitLab => "https://gitlab.com/api/v4".to_string(),
                            RepoPlatform::GitLabSelfHosted => {
                                let host = inferred_host.clone().ok_or_else(|| {
                                    format!(
                                        "git.repos alias '{}' is missing api_base_url and the GitLab host could not be inferred from repo_ref/push_url; set api_base_url explicitly",
                                        alias
                                    )
                                })?;
                                format!("https://{host}/api/v4")
                            }
                            RepoPlatform::GitHub => unreachable!("github handled above"),
                        },
                    };
                    let push_url = match repo.publish.push_url.clone() {
                        Some(push_url) => push_url,
                        None => {
                            let host = match repo.platform {
                                RepoPlatform::GitLab => "gitlab.com".to_string(),
                                RepoPlatform::GitLabSelfHosted => inferred_host.ok_or_else(|| {
                                    format!(
                                        "git.repos alias '{}' is missing push_url and the GitLab host could not be inferred from repo_ref/push_url; set push_url explicitly",
                                        alias
                                    )
                                })?,
                                RepoPlatform::GitHub => unreachable!("github handled above"),
                            };
                            format!("https://{host}/{project_path}.git")
                        }
                    };

                    Ok(PublishRuntimePlan::GitLab(GitLabPublisherConfig {
                        api_base_url,
                        project_path,
                        base_branch,
                        branch_prefix: repo.publish.branch_prefix.clone(),
                        push_url,
                        token,
                        curl_bin: repo.publish.curl_bin.clone(),
                        git_user_name: account.git_user_name.clone(),
                        git_user_email: account.git_user_email.clone(),
                    }))
                }
            };
        }

        let legacy = self.publishing.as_ref().ok_or_else(|| {
            "publishing configuration is required for publish_policy = on_validation_success"
                .to_string()
        })?;
        Ok(PublishRuntimePlan::GitHub(
            legacy.github_config_for_job(revision)?,
        ))
    }
}

fn load_server_config(config: Option<ServerConfig>) -> Result<ServerRuntimeConfig, String> {
    let config = config.unwrap_or(ServerConfig {
        host: None,
        port: None,
        auth_token: None,
        auth_token_env: None,
    });
    let host = normalize_optional_string(config.host.as_deref(), "server.host")?
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let host = host
        .parse::<IpAddr>()
        .map_err(|e| format!("server.host must be a valid IP address, got '{host}': {e}"))?;
    let auth_token =
        match normalize_optional_string(config.auth_token.as_deref(), "server.auth_token")? {
            Some(token) => Some(token),
            None => {
                let env_name = normalize_optional_string(
                    config.auth_token_env.as_deref(),
                    "server.auth_token_env",
                )?;
                match env_name {
                    Some(name) => {
                        let value = env::var(&name).map_err(|_| {
                            format!("server auth token env var '{name}' is not set")
                        })?;
                        let value = value.trim().to_string();
                        if value.is_empty() {
                            None
                        } else {
                            Some(value)
                        }
                    }
                    None => None,
                }
            }
        };

    Ok(ServerRuntimeConfig {
        bind_addr: SocketAddr::new(host, config.port.unwrap_or(8080)),
        auth_token,
    })
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
    core_load_agent_runtime_config(AgentRuntimeConfigInput {
        provider: config.provider,
        bin: normalize_optional_string(config.bin.as_deref(), "agent.bin")?,
        model: normalize_optional_string(config.model.as_deref(), "agent.model")?,
        auth_file: resolve_optional_config_path(
            config_path,
            config.auth_file.as_deref(),
            "agent.auth_file",
        )?,
        api_key: normalize_optional_string(config.api_key.as_deref(), "agent.api_key")?,
        api_key_env: normalize_optional_string(config.api_key_env.as_deref(), "agent.api_key_env")?,
        legacy_codex_bin: normalize_optional_string(
            config.codex_bin.as_deref(),
            "agent.codex_bin",
        )?,
        legacy_codex_auth_file: resolve_optional_config_path(
            config_path,
            config.codex_auth_file.as_deref(),
            "agent.codex_auth_file",
        )?,
        egress_proxy: config.egress_proxy_url,
        egress_allowed_domains: normalize_egress_allowed_domains(config.egress_allowed_domains)?,
    })
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
            git_user_name: "OpenOMAN".to_string(),
            git_user_email: "openoman@openoman.invalid".to_string(),
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

pub(crate) fn resolve_agent_execution_spec(
    config: &AgentRuntimeConfig,
) -> Result<AgentExecutionSpec, String> {
    core_resolve_agent_execution_spec(config)
}

#[cfg(test)]
fn discover_matching_cursor_auth_cache_path_in_home(api_key: &str, home: &Path) -> Option<PathBuf> {
    openoman_core::agents::discover_matching_cursor_auth_cache_path_in_home(api_key, home)
}

fn load_repo_catalog_config(
    git: &GitConfig,
    config_path: &Path,
) -> Result<RepoCatalogRuntimeConfig, String> {
    let mut accounts_by_alias = HashMap::new();
    for account in git.accounts.as_deref().unwrap_or(&[]) {
        let alias = normalize_required_string(&account.alias, "git.accounts.alias")?;
        if accounts_by_alias.contains_key(&alias) {
            return Err(format!("duplicate git account alias '{}'", alias));
        }
        let token = resolve_generic_token(account.token_env.as_deref(), account.token.clone());
        let git_user_name = normalize_required_string(
            account.git_user_name.as_deref().unwrap_or(""),
            "git.accounts.git_user_name",
        )?;
        let git_user_email = normalize_required_string(
            account.git_user_email.as_deref().unwrap_or(""),
            "git.accounts.git_user_email",
        )?;
        accounts_by_alias.insert(
            alias,
            GitAccountRuntimeConfig {
                token,
                git_user_name,
                git_user_email,
            },
        );
    }

    let mut repos_by_alias = HashMap::new();
    for repo in git.repos.as_deref().unwrap_or(&[]) {
        let alias = normalize_required_string(&repo.alias, "git.repos.alias")?;
        if repos_by_alias.contains_key(&alias) {
            return Err(format!("duplicate git repo alias '{}'", alias));
        }
        let repo_ref = normalize_required_string(&repo.repo_ref, "git.repos.repo_ref")?;
        let platform = RepoPlatform::parse(repo.platform.as_deref())
            .map_err(|e| format!("invalid platform for git.repos alias '{}': {}", alias, e))?;
        let account_alias = repo.account.as_ref().map(|value| value.trim().to_string());
        if let Some(account_alias) = account_alias.as_deref() {
            if account_alias.is_empty() {
                return Err(format!(
                    "git.repos alias '{}' has an empty account reference",
                    alias
                ));
            }
            if !accounts_by_alias.contains_key(account_alias) {
                return Err(format!(
                    "git.repos alias '{}' references unknown git account '{}'",
                    alias, account_alias
                ));
            }
        }
        let env_repo_name = match repo
            .env_repo_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => value.to_string(),
            None => alias.clone(),
        };
        let push_url = repo
            .push_url
            .as_deref()
            .map(|value| resolve_push_url(config_path, value))
            .transpose()?;

        repos_by_alias.insert(
            alias,
            RepoRuntimeConfig {
                repo_ref,
                platform,
                account_alias,
                env_repo_name,
                publish: RepoPublishRuntimeConfig {
                    repo_owner: repo.repo_owner.clone(),
                    repo_name: repo.repo_name.clone(),
                    base_branch: repo.base_branch.clone(),
                    branch_prefix: repo
                        .branch_prefix
                        .clone()
                        .unwrap_or_else(|| "openoman".to_string()),
                    api_base_url: repo.api_base_url.clone(),
                    push_url,
                    curl_bin: repo.curl_bin.clone().unwrap_or_else(|| "curl".to_string()),
                },
            },
        );
    }

    Ok(RepoCatalogRuntimeConfig {
        repos_by_alias,
        accounts_by_alias,
    })
}

fn resolve_generic_token(token_env: Option<&str>, token: Option<String>) -> Option<String> {
    if let Some(token_env) = token_env.map(str::trim).filter(|value| !value.is_empty()) {
        if let Ok(value) = env::var(token_env) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
        if looks_like_github_token(token_env) {
            return Some(token_env.to_string());
        }
    }
    token.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[derive(Debug, Clone)]
struct GitHubRepoIdentity {
    owner: String,
    name: String,
}

#[derive(Debug, Clone)]
struct GitLabProjectIdentity {
    project_path: String,
}

#[derive(Debug, Clone)]
struct GitRemoteLocation {
    host: String,
    path: String,
}

fn infer_github_repo_identity(
    push_url: Option<&str>,
    repo_ref: Option<&str>,
) -> Option<GitHubRepoIdentity> {
    if let Some(identity) = push_url.and_then(parse_github_repo_identity_from_git_ref) {
        return Some(identity);
    }
    repo_ref.and_then(parse_github_repo_identity_from_git_ref)
}

fn parse_github_repo_identity_from_git_ref(raw_ref: &str) -> Option<GitHubRepoIdentity> {
    let candidate = raw_ref.trim();
    if candidate.is_empty() {
        return None;
    }

    if let Some((left, right)) = candidate.split_once(':') {
        if !candidate.contains("://") && !left.is_empty() && !left.contains('/') {
            return parse_github_repo_identity_from_path(right);
        }
    }

    if let Some((_, remainder)) = candidate.split_once("://") {
        let (_, path) = remainder.split_once('/')?;
        return parse_github_repo_identity_from_path(path);
    }

    None
}

fn parse_github_repo_identity_from_path(raw_path: &str) -> Option<GitHubRepoIdentity> {
    let path_without_fragment = raw_path
        .split_once('#')
        .map(|(value, _)| value)
        .unwrap_or(raw_path);
    let path_without_query = path_without_fragment
        .split_once('?')
        .map(|(value, _)| value)
        .unwrap_or(path_without_fragment);

    let segments = path_without_query
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() != 2 {
        return None;
    }

    let owner = segments[0].trim();
    let repo_name = segments[1].trim().trim_end_matches(".git");
    if owner.is_empty() || repo_name.is_empty() {
        return None;
    }

    Some(GitHubRepoIdentity {
        owner: owner.to_string(),
        name: repo_name.to_string(),
    })
}

fn infer_gitlab_project_identity(
    push_url: Option<&str>,
    repo_ref: Option<&str>,
) -> Option<GitLabProjectIdentity> {
    if let Some(identity) = push_url.and_then(parse_gitlab_project_identity_from_git_ref) {
        return Some(identity);
    }
    repo_ref.and_then(parse_gitlab_project_identity_from_git_ref)
}

fn parse_gitlab_project_identity_from_git_ref(raw_ref: &str) -> Option<GitLabProjectIdentity> {
    let location = parse_git_remote_location(raw_ref)?;
    parse_gitlab_project_identity_from_path(&location.path)
}

fn parse_gitlab_project_identity_from_path(raw_path: &str) -> Option<GitLabProjectIdentity> {
    let path_without_fragment = raw_path
        .split_once('#')
        .map(|(value, _)| value)
        .unwrap_or(raw_path);
    let path_without_query = path_without_fragment
        .split_once('?')
        .map(|(value, _)| value)
        .unwrap_or(path_without_fragment);

    let segments = path_without_query
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return None;
    }

    let mut normalized = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        let trimmed = if index == segments.len() - 1 {
            segment.trim_end_matches(".git")
        } else {
            segment.trim()
        };
        if trimmed.is_empty() {
            return None;
        }
        normalized.push(trimmed);
    }

    Some(GitLabProjectIdentity {
        project_path: normalized.join("/"),
    })
}

fn infer_git_remote_location(
    push_url: Option<&str>,
    repo_ref: Option<&str>,
) -> Option<GitRemoteLocation> {
    if let Some(location) = push_url.and_then(parse_git_remote_location) {
        return Some(location);
    }
    repo_ref.and_then(parse_git_remote_location)
}

fn parse_git_remote_location(raw_ref: &str) -> Option<GitRemoteLocation> {
    let candidate = raw_ref.trim();
    if candidate.is_empty() {
        return None;
    }

    if let Some((scheme, remainder)) = candidate.split_once("://") {
        if scheme.is_empty() {
            return None;
        }
        let (authority, path) = remainder.split_once('/')?;
        let host = normalize_remote_authority(authority)?;
        return Some(GitRemoteLocation {
            host,
            path: path.to_string(),
        });
    }

    if let Some((left, right)) = candidate.split_once(':') {
        if !left.is_empty() && !left.contains('/') {
            let host = left
                .rsplit_once('@')
                .map(|(_, host)| host)
                .unwrap_or(left)
                .trim();
            if host.is_empty() {
                return None;
            }
            return Some(GitRemoteLocation {
                host: host.to_string(),
                path: right.to_string(),
            });
        }
    }

    None
}

fn normalize_remote_authority(authority: &str) -> Option<String> {
    let trimmed = authority.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_user = trimmed
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(trimmed);
    let without_path = without_user.trim_matches('/');
    if without_path.is_empty() {
        None
    } else {
        Some(without_path.to_string())
    }
}

fn normalize_required_string(raw: &str, field_name: &str) -> Result<String, String> {
    let normalized = raw.trim();
    if normalized.is_empty() {
        return Err(format!("{field_name} must not be empty"));
    }
    Ok(normalized.to_string())
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
    core_validate_agent_networking_contract(firecracker, agent)
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
        assert_eq!(loaded.agent.provider, "cursor");
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
            provider: "cursor".to_string(),
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
            provider: "cursor".to_string(),
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
repo_name = "my_repo"
github_token_env = "github_pat_example123"
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        let publishing = loaded.publishing.expect("publishing config");
        assert_eq!(publishing.token.as_deref(), Some("github_pat_example123"));
    }

    #[test]
    fn app_config_accepts_raw_github_token_in_git_account_token_env_field() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "demo-account"
token_env = "github_pat_example123"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "demo"
repo_ref = "https://github.com/acme/demo.git"
platform = "github"
account = "demo-account"
repo_owner = "acme"
repo_name = "demo"

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
            loaded.resolve_clone_token_for_job(Some("demo")).as_deref(),
            Some("github_pat_example123")
        );
    }

    #[test]
    fn app_config_resolves_clone_token_for_gitlab_alias() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "gitlab-account"
token = "glpat-example123"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "gitlab-demo"
repo_ref = "https://gitlab.com/group/demo.git"
platform = "gitlab"
account = "gitlab-account"

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
            loaded
                .resolve_clone_token_for_job(Some("gitlab-demo"))
                .as_deref(),
            Some("glpat-example123")
        );
    }

    #[test]
    fn app_config_clone_token_for_raw_jobs_falls_back_to_legacy_publishing_token() {
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

[publishing]
provider = "github"
repo_owner = "antonguzun"
repo_name = "my_repo"
github_token_env = "github_pat_example123"
"#,
        )
        .expect("write config");

        let loaded = AppConfig::load(&config_path).expect("load config");
        assert_eq!(
            loaded.resolve_clone_token_for_job(None).as_deref(),
            Some("github_pat_example123")
        );
    }

    #[test]
    fn app_config_resolves_repo_alias_and_env_overlay_dir() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::create_dir_all(temp.path().join("env_for_repo/demo")).expect("create env dir");
        fs::write(temp.path().join("env_for_repo/demo/.env"), "DEMO=1\n").expect("write env");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"
env_for_repo_dir = "./env_for_repo"

[[git.accounts]]
alias = "demo-account"
token = "github_pat_demo_token"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "demo"
repo_ref = "https://github.com/acme/demo.git"
platform = "github"
account = "demo-account"
repo_owner = "acme"
repo_name = "demo"

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

        let resolved = loaded.resolve_submit_repo("demo").expect("resolve alias");
        assert_eq!(resolved.repo_ref, "https://github.com/acme/demo.git");
        assert_eq!(resolved.repo_alias.as_deref(), Some("demo"));

        let fallback = loaded
            .resolve_submit_repo("https://github.com/acme/raw.git")
            .expect("resolve raw repo");
        assert_eq!(fallback.repo_ref, "https://github.com/acme/raw.git");
        assert!(fallback.repo_alias.is_none());

        let overlay = loaded
            .env_overlay_dir_for_alias(Some("demo"))
            .expect("overlay path");
        assert_eq!(overlay, temp.path().join("env_for_repo/demo"));
    }

    #[test]
    fn app_config_publish_plan_uses_repo_account_identity_for_github_alias() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "demo-account"
token = "github_pat_demo_token"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "demo"
repo_ref = "https://github.com/acme/demo.git"
platform = "github"
account = "demo-account"
repo_owner = "acme"
repo_name = "demo"
api_base_url = "https://api.github.com"
branch_prefix = "openoman"

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
        let revision = Revision::new("main").expect("revision");
        let plan = loaded
            .resolve_publish_plan_for_job(Some("demo"), &revision)
            .expect("publish plan");
        let PublishRuntimePlan::GitHub(github) = plan else {
            panic!("expected github publish plan");
        };

        assert_eq!(github.repo_owner, "acme");
        assert_eq!(github.repo_name, "demo");
        assert_eq!(github.base_branch, "main");
        assert_eq!(github.token, "github_pat_demo_token");
        assert_eq!(github.git_user_name, "Repo Bot");
        assert_eq!(github.git_user_email, "repo-bot@example.test");
    }

    #[test]
    fn app_config_publish_plan_infers_repo_owner_and_name_from_repo_ref_or_push_url() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "demo-account"
token = "github_pat_demo_token"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "from-ref"
repo_ref = "https://github.com/acme/inferred-from-ref.git"
platform = "github"
account = "demo-account"

[[git.repos]]
alias = "from-push-url"
repo_ref = "./fixtures/local-repo"
platform = "github"
account = "demo-account"
push_url = "git@github.com:acme/inferred-from-push.git"

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
        let revision = Revision::new("main").expect("revision");

        let from_ref = loaded
            .resolve_publish_plan_for_job(Some("from-ref"), &revision)
            .expect("publish plan");
        let PublishRuntimePlan::GitHub(from_ref) = from_ref else {
            panic!("expected github publish plan for repo_ref inference");
        };
        assert_eq!(from_ref.repo_owner, "acme");
        assert_eq!(from_ref.repo_name, "inferred-from-ref");

        let from_push_url = loaded
            .resolve_publish_plan_for_job(Some("from-push-url"), &revision)
            .expect("publish plan");
        let PublishRuntimePlan::GitHub(from_push_url) = from_push_url else {
            panic!("expected github publish plan for push_url inference");
        };
        assert_eq!(from_push_url.repo_owner, "acme");
        assert_eq!(from_push_url.repo_name, "inferred-from-push");
    }

    #[test]
    fn app_config_publish_plan_requires_explicit_owner_and_name_when_inference_is_ambiguous() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "demo-account"
token = "github_pat_demo_token"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "ambiguous"
repo_ref = "https://github.com/acme/team/demo.git"
platform = "github"
account = "demo-account"

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
        let revision = Revision::new("main").expect("revision");
        let err = loaded
            .resolve_publish_plan_for_job(Some("ambiguous"), &revision)
            .expect_err("publish plan should fail");
        assert!(err.contains("could not be inferred"));
    }

    #[test]
    fn app_config_publish_plan_resolves_gitlab_cloud_and_missing_token_warning() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "gitlab-account"
token = "glpat-example123"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "gitlab-repo"
repo_ref = "https://gitlab.example.test/group/project.git"
platform = "gitlab"
account = "gitlab-account"

[[git.accounts]]
alias = "missing-token-account"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "github-no-token"
repo_ref = "https://github.com/acme/demo.git"
platform = "github"
account = "missing-token-account"
repo_owner = "acme"
repo_name = "demo"

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
        let revision = Revision::new("main").expect("revision");

        let gitlab_plan = loaded
            .resolve_publish_plan_for_job(Some("gitlab-repo"), &revision)
            .expect("publish plan");
        let PublishRuntimePlan::GitLab(gitlab) = gitlab_plan else {
            panic!("expected gitlab publish plan");
        };
        assert_eq!(gitlab.api_base_url, "https://gitlab.com/api/v4");
        assert_eq!(gitlab.project_path, "group/project");
        assert_eq!(gitlab.push_url, "https://gitlab.com/group/project.git");
        assert_eq!(gitlab.token, "glpat-example123");

        let missing_token_plan = loaded
            .resolve_publish_plan_for_job(Some("github-no-token"), &revision)
            .expect("publish plan");
        let PublishRuntimePlan::SkipWithWarning(missing_token_warning) = missing_token_plan else {
            panic!("expected warning for missing token");
        };
        assert!(missing_token_warning.contains("no token"));
    }

    #[test]
    fn app_config_publish_plan_resolves_gitlab_self_hosted_repo() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[[git.accounts]]
alias = "gitlab-account"
token = "glpat-example123"
git_user_name = "Repo Bot"
git_user_email = "repo-bot@example.test"

[[git.repos]]
alias = "gitlab-self-hosted"
repo_ref = "https://gitlab.example.test/group/subgroup/project.git"
platform = "gitlab_self_hosted"
account = "gitlab-account"

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
        let revision = Revision::new("main").expect("revision");
        let plan = loaded
            .resolve_publish_plan_for_job(Some("gitlab-self-hosted"), &revision)
            .expect("publish plan");
        let PublishRuntimePlan::GitLab(gitlab) = plan else {
            panic!("expected gitlab publish plan");
        };

        assert_eq!(gitlab.api_base_url, "https://gitlab.example.test/api/v4");
        assert_eq!(gitlab.project_path, "group/subgroup/project");
        assert_eq!(
            gitlab.push_url,
            "https://gitlab.example.test/group/subgroup/project.git"
        );
        assert_eq!(gitlab.base_branch, "main");
    }

    #[test]
    fn app_config_loads_server_defaults() {
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

        assert_eq!(loaded.server.bind_addr.to_string(), "127.0.0.1:8080");
        assert!(loaded.server.auth_token.is_none());
    }

    #[test]
    fn app_config_reads_server_auth_token_from_env() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[core]
database_path = "./data/openoman.db"

[git]
trusted_workspace_dir = "./workspaces/trusted"

[server]
host = "127.0.0.1"
port = 9090
auth_token_env = "OPENOMAN_TEST_SERVER_TOKEN"

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

        env::set_var("OPENOMAN_TEST_SERVER_TOKEN", "secret-token");
        let loaded = AppConfig::load(&config_path).expect("load config");
        env::remove_var("OPENOMAN_TEST_SERVER_TOKEN");

        assert_eq!(loaded.server.bind_addr.to_string(), "127.0.0.1:9090");
        assert_eq!(loaded.server.auth_token.as_deref(), Some("secret-token"));
    }
}
