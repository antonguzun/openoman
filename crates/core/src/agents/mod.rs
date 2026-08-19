use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use serde::Deserialize;

use crate::execution::{
    AgentExecutionSpec, ExecutionError, FirecrackerBackendConfig, FirecrackerNetworkingMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentReportMode {
    File,
    Stdout,
}

#[derive(Debug, Clone)]
pub struct AgentLaunchPlan {
    pub provider_id: &'static str,
    pub binary: String,
    pub args: Vec<String>,
    pub working_directory: String,
    pub report_mode: AgentReportMode,
    pub auth_file_home_relative_path: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub version_probe_args: Vec<String>,
    pub redact_api_key_args: bool,
}

#[derive(Debug, Clone)]
pub struct AgentLaunchContext<'a> {
    pub binary: &'a str,
    pub workspace_dir: &'a str,
    pub report_path: &'a str,
    pub instruction: &'a str,
}

#[derive(Clone)]
pub struct AgentRuntimeConfig {
    pub provider: String,
    pub bin: String,
    pub model: Option<String>,
    pub auth_file: Option<PathBuf>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub egress_proxy: Option<String>,
    pub egress_allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentRuntimeConfigInput {
    pub provider: Option<String>,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub auth_file: Option<PathBuf>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub legacy_codex_bin: Option<String>,
    pub legacy_codex_auth_file: Option<PathBuf>,
    pub egress_proxy: Option<String>,
    pub egress_allowed_domains: Vec<String>,
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

pub fn load_agent_runtime_config(
    input: AgentRuntimeConfigInput,
) -> Result<AgentRuntimeConfig, String> {
    let provider = parse_agent_provider(input.provider.as_deref())?;
    load_agent_runtime_config_with_registry(default_agent_adapter_registry(), provider, input)
}

fn load_agent_runtime_config_with_registry(
    registry: &AgentAdapterRegistry,
    provider: String,
    input: AgentRuntimeConfigInput,
) -> Result<AgentRuntimeConfig, String> {
    let adapter = registry.adapter_for(&provider)?;
    adapter.load_runtime_config(&provider, input)
}

pub fn validate_agent_networking_contract(
    firecracker: Option<&FirecrackerBackendConfig>,
    agent: &AgentRuntimeConfig,
) -> Result<(), String> {
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

    default_agent_adapter_registry()
        .adapter_for(&agent.provider)?
        .validate_host_proxy_egress(&agent.egress_allowed_domains)
}

pub fn resolve_agent_execution_spec(
    config: &AgentRuntimeConfig,
) -> Result<AgentExecutionSpec, String> {
    resolve_agent_execution_spec_with_registry(default_agent_adapter_registry(), config)
}

fn resolve_agent_execution_spec_with_registry(
    registry: &AgentAdapterRegistry,
    config: &AgentRuntimeConfig,
) -> Result<AgentExecutionSpec, String> {
    let (api_key, auth_file) = registry
        .adapter_for(&config.provider)?
        .resolve_execution_inputs(config)?;

    Ok(AgentExecutionSpec {
        provider: config.provider.clone(),
        bin: config.bin.clone(),
        model: config.model.clone(),
        auth_file,
        api_key,
        egress_proxy: config.egress_proxy.clone(),
        egress_allowed_domains: config.egress_allowed_domains.clone(),
    })
}

pub fn build_launch_plan(
    spec: &AgentExecutionSpec,
    context: AgentLaunchContext<'_>,
) -> Result<AgentLaunchPlan, ExecutionError> {
    build_launch_plan_with_registry(default_agent_adapter_registry(), spec, context)
}

fn build_launch_plan_with_registry(
    registry: &AgentAdapterRegistry,
    spec: &AgentExecutionSpec,
    context: AgentLaunchContext<'_>,
) -> Result<AgentLaunchPlan, ExecutionError> {
    registry
        .adapter_for(&spec.provider)
        .map_err(ExecutionError::InvalidConfig)?
        .build_launch_plan(spec, context)
}

pub fn discover_matching_cursor_auth_cache_path(api_key: &str) -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    discover_matching_cursor_auth_cache_path_in_home(api_key, &PathBuf::from(home))
}

pub fn discover_matching_cursor_auth_cache_path_in_home(
    api_key: &str,
    home: &Path,
) -> Option<PathBuf> {
    let auth_path = home.join(".config/cursor/auth.json");
    let contents = fs::read_to_string(&auth_path).ok()?;
    let auth_cache: CursorAuthCache = serde_json::from_str(&contents).ok()?;
    if auth_cache.api_key.as_deref() == Some(api_key) {
        Some(auth_path)
    } else {
        None
    }
}

fn parse_agent_provider(raw: Option<&str>) -> Result<String, String> {
    let normalized = raw.unwrap_or("codex").trim().to_ascii_lowercase();
    if default_agent_adapter_registry().has_adapter(&normalized) {
        return Ok(normalized);
    }

    Err(format!(
        "unsupported agent provider '{}'; supported providers: {}",
        normalized,
        default_agent_adapter_registry().supported_provider_list()
    ))
}

// The guest exports the resolved value verbatim, so surrounding whitespace or a
// trailing CR from a copy-pasted token or CRLF dotenv file would only surface as an
// auth failure deep inside the sandbox; trim it here instead.
fn resolve_api_key_from_config(
    config: &AgentRuntimeConfig,
    provider: &str,
) -> Result<String, String> {
    if let Some(api_key) = &config.api_key {
        return Ok(api_key.trim().to_string());
    }
    let env_name = config.api_key_env.as_deref().ok_or_else(|| {
        format!(
            "agent.api_key or agent.api_key_env is required when agent.provider = \"{provider}\""
        )
    })?;
    let value = env::var(env_name).map_err(|_| {
        format!("agent.api_key_env references missing environment variable {env_name}")
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!(
            "environment variable {env_name} referenced by agent.api_key_env must not be empty"
        ));
    }
    Ok(value.to_string())
}

fn resolve_cursor_execution_inputs(
    config: &AgentRuntimeConfig,
) -> Result<ResolvedCursorExecutionInputs, String> {
    let api_key = resolve_api_key_from_config(config, "cursor")?;
    Ok(ResolvedCursorExecutionInputs {
        staged_auth_file: discover_matching_cursor_auth_cache_path(&api_key),
        api_key,
    })
}

// Shared config loading for the api-key providers (cursor, claude): they accept
// the same provider-neutral keys and differ only in provider name and default
// binary.
fn load_api_key_provider_runtime_config(
    provider: &str,
    default_bin: &str,
    input: AgentRuntimeConfigInput,
) -> Result<AgentRuntimeConfig, String> {
    if input.legacy_codex_bin.is_some() {
        return Err(format!(
            "agent.codex_bin is a Codex-only compatibility field and cannot be set when agent.provider = \"{provider}\""
        ));
    }
    if input.auth_file.is_some() || input.legacy_codex_auth_file.is_some() {
        return Err(format!(
            "agent.auth_file and legacy agent.codex_auth_file are Codex-only fields and cannot be set when agent.provider = \"{provider}\""
        ));
    }
    let (api_key, api_key_env) = match (input.api_key, input.api_key_env) {
        (Some(_), Some(_)) => Err(format!(
            "agent.api_key and agent.api_key_env are mutually exclusive; set only one when agent.provider = \"{provider}\""
        )),
        (Some(api_key), None) => Ok((Some(api_key), None)),
        (None, Some(api_key_env)) => Ok((None, Some(api_key_env))),
        (None, None) => Err(format!(
            "agent.api_key or agent.api_key_env is required when agent.provider = \"{provider}\""
        )),
    }?;

    Ok(AgentRuntimeConfig {
        provider: provider.to_string(),
        bin: input.bin.unwrap_or_else(|| default_bin.to_string()),
        model: input.model,
        auth_file: None,
        api_key,
        api_key_env,
        egress_proxy: input.egress_proxy,
        egress_allowed_domains: input.egress_allowed_domains,
    })
}

fn agent_egress_policy(domains: &[String]) -> AgentEgressPolicy<'_> {
    if domains.iter().any(|domain| domain == "*") {
        AgentEgressPolicy::AllowAllDebug
    } else {
        AgentEgressPolicy::Restricted(domains)
    }
}

// Hostname a provider's CLI must be able to reach through the host CONNECT
// proxy in host-proxy networking mode, plus the reason quoted in the error.
struct RequiredHostProxyDomain {
    domain: &'static str,
    reason: &'static str,
}

trait AgentAdapter {
    fn provider_id(&self) -> &'static str;
    fn load_runtime_config(
        &self,
        provider: &str,
        input: AgentRuntimeConfigInput,
    ) -> Result<AgentRuntimeConfig, String>;
    fn resolve_execution_inputs(
        &self,
        config: &AgentRuntimeConfig,
    ) -> Result<(Option<String>, Option<PathBuf>), String>;
    fn required_host_proxy_domain(&self) -> Option<RequiredHostProxyDomain> {
        None
    }
    fn validate_host_proxy_egress(&self, egress_allowed_domains: &[String]) -> Result<(), String> {
        let Some(required) = self.required_host_proxy_domain() else {
            return Ok(());
        };
        match agent_egress_policy(egress_allowed_domains) {
            AgentEgressPolicy::AllowAllDebug => Ok(()),
            AgentEgressPolicy::Restricted(domains)
                if domains.iter().any(|domain| domain == required.domain) =>
            {
                Ok(())
            }
            AgentEgressPolicy::Restricted(_) => Err(format!(
                "agent.egress_allowed_domains must include \"{}\" when agent.provider = \"{}\" and sandbox.firecracker.network.mode = \"host-proxy\" because {}",
                required.domain,
                self.provider_id(),
                required.reason
            )),
        }
    }
    fn build_launch_plan(
        &self,
        spec: &AgentExecutionSpec,
        context: AgentLaunchContext<'_>,
    ) -> Result<AgentLaunchPlan, ExecutionError>;
}

struct CodexAdapter;
struct CursorAdapter;
struct ClaudeAdapter;

#[derive(Default)]
struct AgentAdapterRegistry {
    adapters: HashMap<String, Box<dyn AgentAdapter + Send + Sync>>,
}

impl AgentAdapterRegistry {
    fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(CodexAdapter);
        registry.register(CursorAdapter);
        registry.register(ClaudeAdapter);
        registry
    }

    fn register<T>(&mut self, adapter: T)
    where
        T: AgentAdapter + Send + Sync + 'static,
    {
        self.adapters
            .insert(adapter.provider_id().to_string(), Box::new(adapter));
    }

    fn adapter_for(&self, provider_id: &str) -> Result<&(dyn AgentAdapter + Send + Sync), String> {
        self.adapters
            .get(provider_id)
            .map(|adapter| adapter.as_ref())
            .ok_or_else(|| {
                format!(
                    "unsupported agent provider '{}'; supported providers: {}",
                    provider_id,
                    self.supported_provider_list()
                )
            })
    }

    fn has_adapter(&self, provider_id: &str) -> bool {
        self.adapters.contains_key(provider_id)
    }

    fn supported_provider_list(&self) -> String {
        let mut providers: Vec<&str> = self.adapters.keys().map(String::as_str).collect();
        providers.sort_unstable();
        providers.join(", ")
    }
}

fn default_agent_adapter_registry() -> &'static AgentAdapterRegistry {
    static REGISTRY: OnceLock<AgentAdapterRegistry> = OnceLock::new();
    REGISTRY.get_or_init(AgentAdapterRegistry::with_defaults)
}

impl AgentAdapter for CodexAdapter {
    fn provider_id(&self) -> &'static str {
        "codex"
    }

    fn load_runtime_config(
        &self,
        provider: &str,
        input: AgentRuntimeConfigInput,
    ) -> Result<AgentRuntimeConfig, String> {
        let bin = match (input.bin, input.legacy_codex_bin) {
            (Some(bin), Some(legacy_bin)) if bin != legacy_bin => Err(
                "agent.bin conflicts with legacy agent.codex_bin; set only one value or make them identical"
                    .to_string(),
            ),
            (Some(bin), _) => Ok(bin),
            (None, Some(bin)) => Ok(bin),
            (None, None) => Ok("codex".to_string()),
        }?;
        if input.model.is_some() {
            return Err(
                "agent.model is currently supported only when agent.provider = \"cursor\" or \"claude\""
                    .to_string(),
            );
        }
        if input.api_key.is_some() || input.api_key_env.is_some() {
            return Err(
                "agent.api_key and agent.api_key_env are Cursor-only and Claude-only fields and cannot be set when agent.provider = \"codex\""
                    .to_string(),
            );
        }
        let auth_file = match (input.auth_file, input.legacy_codex_auth_file) {
            (Some(auth_file), Some(legacy_auth_file)) if auth_file != legacy_auth_file => Err(
                "agent.auth_file conflicts with legacy agent.codex_auth_file; set only one value or make them identical"
                    .to_string(),
            ),
            (Some(auth_file), _) => Ok(Some(auth_file)),
            (None, Some(auth_file)) => Ok(Some(auth_file)),
            (None, None) => Ok(None),
        }?;

        Ok(AgentRuntimeConfig {
            provider: provider.to_string(),
            bin,
            model: None,
            auth_file,
            api_key: None,
            api_key_env: None,
            egress_proxy: input.egress_proxy,
            egress_allowed_domains: input.egress_allowed_domains,
        })
    }

    fn resolve_execution_inputs(
        &self,
        config: &AgentRuntimeConfig,
    ) -> Result<(Option<String>, Option<PathBuf>), String> {
        Ok((None, config.auth_file.clone()))
    }

    fn build_launch_plan(
        &self,
        spec: &AgentExecutionSpec,
        context: AgentLaunchContext<'_>,
    ) -> Result<AgentLaunchPlan, ExecutionError> {
        Ok(AgentLaunchPlan {
            provider_id: self.provider_id(),
            binary: context.binary.to_string(),
            args: vec![
                "exec".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--color".to_string(),
                "never".to_string(),
                "-C".to_string(),
                context.workspace_dir.to_string(),
                "-o".to_string(),
                context.report_path.to_string(),
                context.instruction.to_string(),
            ],
            working_directory: context.workspace_dir.to_string(),
            report_mode: AgentReportMode::File,
            auth_file_home_relative_path: spec
                .auth_file
                .as_ref()
                .map(|_| PathBuf::from(".codex").join("auth.json")),
            env: Vec::new(),
            version_probe_args: vec!["--version".to_string()],
            redact_api_key_args: false,
        })
    }
}

impl AgentAdapter for CursorAdapter {
    fn provider_id(&self) -> &'static str {
        "cursor"
    }

    fn load_runtime_config(
        &self,
        provider: &str,
        input: AgentRuntimeConfigInput,
    ) -> Result<AgentRuntimeConfig, String> {
        load_api_key_provider_runtime_config(provider, "cursor-agent", input)
    }

    fn resolve_execution_inputs(
        &self,
        config: &AgentRuntimeConfig,
    ) -> Result<(Option<String>, Option<PathBuf>), String> {
        let resolved = resolve_cursor_execution_inputs(config)?;
        Ok((Some(resolved.api_key), resolved.staged_auth_file))
    }

    fn required_host_proxy_domain(&self) -> Option<RequiredHostProxyDomain> {
        Some(RequiredHostProxyDomain {
            domain: "api2.cursor.sh",
            reason: "Cursor CLI print mode tunnels through that hostname",
        })
    }

    fn build_launch_plan(
        &self,
        spec: &AgentExecutionSpec,
        context: AgentLaunchContext<'_>,
    ) -> Result<AgentLaunchPlan, ExecutionError> {
        let mut args = Vec::new();
        let mut env = Vec::new();
        if spec.auth_file.is_none() {
            let api_key = spec.api_key.as_ref().ok_or_else(|| {
                ExecutionError::InvalidConfig(
                    "cursor execution requires an api key or staged auth file".to_string(),
                )
            })?;
            env.push(("CURSOR_API_KEY".to_string(), api_key.clone()));
            args.push("--api-key".to_string());
            args.push(api_key.clone());
        }
        args.extend([
            "-p".to_string(),
            "-f".to_string(),
            "--output-format".to_string(),
            "text".to_string(),
        ]);
        if let Some(model) = &spec.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        // The instruction is free text and may start with '-'; end option parsing
        // so the CLI always treats it as the positional prompt.
        args.push("--".to_string());
        args.push(context.instruction.to_string());

        Ok(AgentLaunchPlan {
            provider_id: self.provider_id(),
            binary: context.binary.to_string(),
            args,
            working_directory: context.workspace_dir.to_string(),
            report_mode: AgentReportMode::Stdout,
            auth_file_home_relative_path: spec
                .auth_file
                .as_ref()
                .map(|_| PathBuf::from(".config").join("cursor").join("auth.json")),
            env,
            version_probe_args: vec!["--version".to_string()],
            redact_api_key_args: spec.auth_file.is_none(),
        })
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn provider_id(&self) -> &'static str {
        "claude"
    }

    fn load_runtime_config(
        &self,
        provider: &str,
        input: AgentRuntimeConfigInput,
    ) -> Result<AgentRuntimeConfig, String> {
        load_api_key_provider_runtime_config(provider, "claude", input)
    }

    fn resolve_execution_inputs(
        &self,
        config: &AgentRuntimeConfig,
    ) -> Result<(Option<String>, Option<PathBuf>), String> {
        let token = resolve_api_key_from_config(config, "claude")?;
        Ok((Some(token), None))
    }

    fn required_host_proxy_domain(&self) -> Option<RequiredHostProxyDomain> {
        Some(RequiredHostProxyDomain {
            domain: "api.anthropic.com",
            reason: "Claude Code reaches the Anthropic API through that hostname",
        })
    }

    fn build_launch_plan(
        &self,
        spec: &AgentExecutionSpec,
        context: AgentLaunchContext<'_>,
    ) -> Result<AgentLaunchPlan, ExecutionError> {
        let token = spec.api_key.as_ref().ok_or_else(|| {
            ExecutionError::InvalidConfig(
                "claude execution requires an api key or oauth token".to_string(),
            )
        })?;

        let mut args = vec![
            "-p".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--output-format".to_string(),
            "text".to_string(),
        ];
        if let Some(model) = &spec.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        // The instruction is free text and may start with '-'; end option parsing
        // so the CLI always treats it as the positional prompt.
        args.push("--".to_string());
        args.push(context.instruction.to_string());

        Ok(AgentLaunchPlan {
            provider_id: self.provider_id(),
            binary: context.binary.to_string(),
            args,
            working_directory: context.workspace_dir.to_string(),
            report_mode: AgentReportMode::Stdout,
            auth_file_home_relative_path: None,
            env: vec![
                // Subscription OAuth token. The guest starts from an empty environment and
                // only sources the generated agent.env, so a host ANTHROPIC_API_KEY cannot
                // leak in and take precedence over this token.
                ("CLAUDE_CODE_OAUTH_TOKEN".to_string(), token.clone()),
                // IS_SANDBOX is deliberately NOT set here: whether the agent is sandboxed
                // is a property of the backend, not the provider. The Firecracker backend
                // sets it when rendering agent.env; the process backend runs the agent on
                // the host, where forging it would defeat the CLI's own root guard.
                //
                // The version probe is wrapped in `timeout -k 1 10`. An auto-update or
                // telemetry request to a host outside the egress allowlist would hang past
                // that deadline and abort the attempt before the agent ever runs.
                ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
                (
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                    "1".to_string(),
                ),
            ],
            version_probe_args: vec!["--version".to_string()],
            // The token travels in the environment only. The guest log redactor understands
            // just a literal `--api-key` argument, so putting it in argv would write it to
            // the stored sandbox.logs artifact in clear text.
            redact_api_key_args: false,
        })
    }
}

pub fn auth_install_path(home_dir: &Path, relative: &Path) -> PathBuf {
    home_dir.join(relative)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{FirecrackerNetworkingConfig, FirecrackerNetworkingMode};
    use tempfile::TempDir;

    fn codex_spec() -> AgentExecutionSpec {
        AgentExecutionSpec {
            provider: "codex".to_string(),
            bin: "codex".to_string(),
            model: None,
            auth_file: Some(PathBuf::from("/host/.codex/auth.json")),
            api_key: None,
            egress_proxy: None,
            egress_allowed_domains: Vec::new(),
        }
    }

    fn cursor_spec() -> AgentExecutionSpec {
        AgentExecutionSpec {
            provider: "cursor".to_string(),
            bin: "cursor-agent".to_string(),
            model: Some("gpt-5".to_string()),
            auth_file: None,
            api_key: Some("cursor-secret".to_string()),
            egress_proxy: None,
            egress_allowed_domains: vec!["api2.cursor.sh".to_string()],
        }
    }

    #[test]
    fn codex_adapter_builds_file_report_plan() {
        let plan = build_launch_plan(
            &codex_spec(),
            AgentLaunchContext {
                binary: "/usr/local/bin/codex",
                workspace_dir: "/workspace",
                report_path: "/report.txt",
                instruction: "append blank line",
            },
        )
        .expect("plan");

        assert_eq!(plan.provider_id, "codex");
        assert_eq!(plan.binary, "/usr/local/bin/codex");
        assert_eq!(plan.report_mode, AgentReportMode::File);
        assert_eq!(
            plan.auth_file_home_relative_path.as_deref(),
            Some(Path::new(".codex/auth.json"))
        );
        assert!(plan.args.contains(&"/report.txt".to_string()));
    }

    fn claude_spec() -> AgentExecutionSpec {
        AgentExecutionSpec {
            provider: "claude".to_string(),
            bin: "claude".to_string(),
            model: Some("claude-opus-4-6".to_string()),
            auth_file: None,
            api_key: Some("claude-secret".to_string()),
            egress_proxy: None,
            egress_allowed_domains: vec!["api.anthropic.com".to_string()],
        }
    }

    #[test]
    fn cursor_adapter_builds_stdout_plan_with_redacted_api_key() {
        let plan = build_launch_plan(
            &cursor_spec(),
            AgentLaunchContext {
                binary: "/usr/local/bin/cursor-agent",
                workspace_dir: "/workspace",
                report_path: "/report.txt",
                instruction: "append blank line",
            },
        )
        .expect("plan");

        assert_eq!(plan.provider_id, "cursor");
        assert_eq!(plan.report_mode, AgentReportMode::Stdout);
        assert!(plan.redact_api_key_args);
        assert!(plan.args.contains(&"--api-key".to_string()));
        assert_eq!(
            plan.args.get(plan.args.len() - 2).map(String::as_str),
            Some("--")
        );
        assert!(plan
            .env
            .iter()
            .any(|(key, value)| key == "CURSOR_API_KEY" && value == "cursor-secret"));
    }

    #[test]
    fn load_cursor_runtime_config_rejects_codex_compat_fields() {
        let err = load_agent_runtime_config(AgentRuntimeConfigInput {
            provider: Some("cursor".to_string()),
            legacy_codex_bin: Some("/usr/local/bin/codex".to_string()),
            api_key: Some("cursor-secret".to_string()),
            ..AgentRuntimeConfigInput::default()
        })
        .expect_err("cursor should reject codex compatibility fields");

        assert!(err.contains("agent.codex_bin"));
        assert!(err.contains("cursor"));
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
    fn claude_adapter_builds_stdout_plan_with_token_in_env() {
        let plan = build_launch_plan(
            &claude_spec(),
            AgentLaunchContext {
                binary: "/usr/local/bin/claude",
                workspace_dir: "/workspace",
                report_path: "/report.txt",
                instruction: "append blank line",
            },
        )
        .expect("plan");

        assert_eq!(plan.provider_id, "claude");
        assert_eq!(plan.report_mode, AgentReportMode::Stdout);
        assert!(plan.auth_file_home_relative_path.is_none());
        assert!(plan.args.contains(&"-p".to_string()));
        assert!(plan
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
        assert!(plan.args.contains(&"claude-opus-4-6".to_string()));
        assert_eq!(
            plan.args.last().map(String::as_str),
            Some("append blank line")
        );
        // A dash-leading instruction must stay positional.
        assert_eq!(
            plan.args.get(plan.args.len() - 2).map(String::as_str),
            Some("--")
        );
        assert!(plan
            .env
            .iter()
            .any(|(key, value)| key == "CLAUDE_CODE_OAUTH_TOKEN" && value == "claude-secret"));
    }

    #[test]
    fn claude_launch_plan_keeps_token_out_of_argv() {
        let plan = build_launch_plan(
            &claude_spec(),
            AgentLaunchContext {
                binary: "/usr/local/bin/claude",
                workspace_dir: "/workspace",
                report_path: "/report.txt",
                instruction: "append blank line",
            },
        )
        .expect("plan");

        // The guest log redactor only understands a literal `--api-key` argument, so the
        // token must never appear in argv or it would be written to sandbox.logs verbatim.
        assert!(!plan.redact_api_key_args);
        assert!(!plan.args.iter().any(|arg| arg.contains("claude-secret")));
    }

    #[test]
    fn claude_launch_plan_leaves_sandbox_marking_to_the_backend() {
        let plan = build_launch_plan(
            &claude_spec(),
            AgentLaunchContext {
                binary: "/usr/local/bin/claude",
                workspace_dir: "/workspace",
                report_path: "/report.txt",
                instruction: "append blank line",
            },
        )
        .expect("plan");

        // Whether the agent is sandboxed is a backend fact: the Firecracker
        // renderer exports IS_SANDBOX=1, while the process backend runs the agent
        // on the host, where a plan-supplied IS_SANDBOX would forge Claude Code's
        // own refusal to run --dangerously-skip-permissions under root.
        assert!(!plan.env.iter().any(|(key, _)| key == "IS_SANDBOX"));
        // The version probe runs under `timeout -k 1 10`; background traffic to hosts
        // outside the egress allowlist would hang past that deadline.
        assert!(plan
            .env
            .iter()
            .any(|(key, value)| key == "DISABLE_AUTOUPDATER" && value == "1"));
    }

    #[test]
    fn resolve_api_key_from_config_trims_inline_and_env_values() {
        let inline = AgentRuntimeConfig {
            provider: "claude".to_string(),
            bin: "claude".to_string(),
            model: None,
            auth_file: None,
            api_key: Some("  sk-ant-oat01-inline\n".to_string()),
            api_key_env: None,
            egress_proxy: None,
            egress_allowed_domains: Vec::new(),
        };
        assert_eq!(
            resolve_api_key_from_config(&inline, "claude").expect("inline key"),
            "sk-ant-oat01-inline"
        );

        // A CRLF dotenv file or a copy-paste leaves whitespace around the value.
        env::set_var("OPENOMAN_TEST_CLAUDE_TOKEN_TRIM", " sk-ant-oat01-env\r\n");
        let from_env = AgentRuntimeConfig {
            api_key: None,
            api_key_env: Some("OPENOMAN_TEST_CLAUDE_TOKEN_TRIM".to_string()),
            ..inline
        };
        assert_eq!(
            resolve_api_key_from_config(&from_env, "claude").expect("env key"),
            "sk-ant-oat01-env"
        );
        env::remove_var("OPENOMAN_TEST_CLAUDE_TOKEN_TRIM");
    }

    #[test]
    fn load_claude_runtime_config_rejects_codex_compat_fields() {
        let err = load_agent_runtime_config(AgentRuntimeConfigInput {
            provider: Some("claude".to_string()),
            api_key: Some("claude-secret".to_string()),
            legacy_codex_bin: Some("codex".to_string()),
            ..AgentRuntimeConfigInput::default()
        })
        .expect_err("codex compatibility fields must be rejected for claude");
        assert!(err.contains("codex_bin"));

        let err = load_agent_runtime_config(AgentRuntimeConfigInput {
            provider: Some("claude".to_string()),
            ..AgentRuntimeConfigInput::default()
        })
        .expect_err("claude requires a token");
        assert!(err.contains("agent.api_key or agent.api_key_env"));
    }

    #[test]
    fn load_claude_runtime_config_defaults_bin_and_accepts_model() {
        let config = load_agent_runtime_config(AgentRuntimeConfigInput {
            provider: Some("claude".to_string()),
            model: Some("claude-opus-4-6".to_string()),
            api_key: Some("claude-secret".to_string()),
            ..AgentRuntimeConfigInput::default()
        })
        .expect("runtime config");

        assert_eq!(config.bin, "claude");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn validate_claude_host_proxy_requires_api_anthropic_com() {
        let firecracker = FirecrackerBackendConfig {
            mode: crate::execution::FirecrackerMode::Direct,
            firecracker_bin: "firecracker".to_string(),
            jailer_bin: "jailer".to_string(),
            kernel_image_path: PathBuf::from("/kernel"),
            rootfs_image_path: PathBuf::from("/rootfs"),
            runtime_disk_bytes: None,
            guest_cid_base: 10_000,
            docker_daemon: false,
            docker_auth_config: None,
            user_package_dirs: Vec::new(),
            networking: FirecrackerNetworkingConfig {
                mode: FirecrackerNetworkingMode::HostProxy,
                privilege_mode: crate::execution::FirecrackerNetworkPrivilegeMode::Sudo,
                tap_name_prefix: "oomtap".to_string(),
                proxy_port: 3128,
                allowed_connect_ports: vec![443],
                subnet_cidr: "172.22.0.0/16".to_string(),
            },
        };
        let agent = AgentRuntimeConfig {
            provider: "claude".to_string(),
            bin: "claude".to_string(),
            model: None,
            auth_file: None,
            api_key: Some("claude-secret".to_string()),
            api_key_env: None,
            egress_proxy: None,
            egress_allowed_domains: vec!["example.com".to_string()],
        };

        let err = validate_agent_networking_contract(Some(&firecracker), &agent)
            .expect_err("claude host proxy should require api.anthropic.com");
        assert!(err.contains("api.anthropic.com"));
    }

    #[test]
    fn validate_cursor_host_proxy_requires_api2_cursor_sh() {
        let firecracker = FirecrackerBackendConfig {
            mode: crate::execution::FirecrackerMode::Direct,
            firecracker_bin: "firecracker".to_string(),
            jailer_bin: "jailer".to_string(),
            kernel_image_path: PathBuf::from("/kernel"),
            rootfs_image_path: PathBuf::from("/rootfs"),
            runtime_disk_bytes: None,
            guest_cid_base: 10_000,
            docker_daemon: false,
            docker_auth_config: None,
            user_package_dirs: Vec::new(),
            networking: FirecrackerNetworkingConfig {
                mode: FirecrackerNetworkingMode::HostProxy,
                privilege_mode: crate::execution::FirecrackerNetworkPrivilegeMode::Sudo,
                tap_name_prefix: "oomtap".to_string(),
                proxy_port: 3128,
                allowed_connect_ports: vec![443],
                subnet_cidr: "172.22.0.0/16".to_string(),
            },
        };
        let agent = AgentRuntimeConfig {
            provider: "cursor".to_string(),
            bin: "cursor-agent".to_string(),
            model: None,
            auth_file: None,
            api_key: Some("cursor-secret".to_string()),
            api_key_env: None,
            egress_proxy: None,
            egress_allowed_domains: vec!["api.cursor.com".to_string()],
        };

        let err = validate_agent_networking_contract(Some(&firecracker), &agent)
            .expect_err("cursor host proxy should require api2.cursor.sh");
        assert!(err.contains("api2.cursor.sh"));
    }

    #[test]
    fn adapter_registry_supports_custom_provider_without_pipeline_changes() {
        struct FakeAdapter;

        impl AgentAdapter for FakeAdapter {
            fn provider_id(&self) -> &'static str {
                "fake"
            }

            fn load_runtime_config(
                &self,
                provider: &str,
                input: AgentRuntimeConfigInput,
            ) -> Result<AgentRuntimeConfig, String> {
                Ok(AgentRuntimeConfig {
                    provider: provider.to_string(),
                    bin: input.bin.unwrap_or_else(|| "fake-agent".to_string()),
                    model: input.model,
                    auth_file: None,
                    api_key: None,
                    api_key_env: None,
                    egress_proxy: input.egress_proxy,
                    egress_allowed_domains: input.egress_allowed_domains,
                })
            }

            fn resolve_execution_inputs(
                &self,
                config: &AgentRuntimeConfig,
            ) -> Result<(Option<String>, Option<PathBuf>), String> {
                Ok((config.api_key.clone(), config.auth_file.clone()))
            }

            fn build_launch_plan(
                &self,
                spec: &AgentExecutionSpec,
                context: AgentLaunchContext<'_>,
            ) -> Result<AgentLaunchPlan, ExecutionError> {
                Ok(AgentLaunchPlan {
                    provider_id: self.provider_id(),
                    binary: context.binary.to_string(),
                    args: vec!["run".to_string(), context.instruction.to_string()],
                    working_directory: context.workspace_dir.to_string(),
                    report_mode: AgentReportMode::Stdout,
                    auth_file_home_relative_path: None,
                    env: vec![
                        ("FAKE_PROVIDER".to_string(), "1".to_string()),
                        (
                            "FAKE_MODEL".to_string(),
                            spec.model.clone().unwrap_or_default(),
                        ),
                    ],
                    version_probe_args: vec!["--version".to_string()],
                    redact_api_key_args: false,
                })
            }
        }

        let mut registry = AgentAdapterRegistry::with_defaults();
        registry.register(FakeAdapter);

        let runtime = load_agent_runtime_config_with_registry(
            &registry,
            "fake".to_string(),
            AgentRuntimeConfigInput {
                bin: Some("/usr/local/bin/fake-agent".to_string()),
                model: Some("fake-model".to_string()),
                ..AgentRuntimeConfigInput::default()
            },
        )
        .expect("runtime config");
        let spec = resolve_agent_execution_spec_with_registry(&registry, &runtime)
            .expect("execution spec");
        let plan = build_launch_plan_with_registry(
            &registry,
            &spec,
            AgentLaunchContext {
                binary: &spec.bin,
                workspace_dir: "/workspace",
                report_path: "/report.txt",
                instruction: "write a summary",
            },
        )
        .expect("launch plan");

        assert_eq!(plan.provider_id, "fake");
        assert_eq!(plan.binary, "/usr/local/bin/fake-agent");
        assert_eq!(plan.report_mode, AgentReportMode::Stdout);
        assert!(plan.args.contains(&"write a summary".to_string()));
    }
}
