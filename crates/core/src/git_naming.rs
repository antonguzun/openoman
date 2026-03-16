use std::process::Command;

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGitNaming {
    pub branch_name: String,
    pub commit_message: String,
}

#[derive(Debug, Clone)]
pub struct GitNamingPromptContext<'a> {
    pub job_id: &'a str,
    pub repo_ref: &'a str,
    pub repo_alias: Option<&'a str>,
    pub revision: &'a str,
    pub instruction: &'a str,
    pub platform: &'a str,
}

#[derive(Debug, Clone)]
pub struct OpenAiGitNamingConfig {
    pub api_base_url: String,
    pub model: String,
    pub api_key: String,
    pub curl_bin: String,
    pub timeout_seconds: u64,
    pub prompt_template: String,
}

#[derive(Debug)]
pub enum GitNamingError {
    Io(std::io::Error),
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    InvalidResponse(String),
}

impl std::fmt::Display for GitNamingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::CommandFailed {
                program,
                args,
                stderr,
            } => write!(
                f,
                "command failed: {} {}\n{}",
                program,
                args.join(" "),
                stderr
            ),
            Self::InvalidResponse(message) => write!(f, "invalid naming response: {message}"),
        }
    }
}

impl std::error::Error for GitNamingError {}

impl From<std::io::Error> for GitNamingError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn resolve_git_naming(
    config: Option<&OpenAiGitNamingConfig>,
    branch_prefix: &str,
    context: &GitNamingPromptContext<'_>,
) -> ResolvedGitNaming {
    if let Some(config) = config {
        if let Ok(candidate) = generate_openai_git_naming(config, context) {
            return candidate;
        }
    }

    build_legacy_git_naming(branch_prefix, context.job_id)
}

pub fn build_legacy_git_naming(branch_prefix: &str, job_id: &str) -> ResolvedGitNaming {
    ResolvedGitNaming {
        branch_name: build_branch_name(branch_prefix, job_id),
        commit_message: format!("OpenOMAN job {job_id}"),
    }
}

pub fn build_branch_name(prefix: &str, raw_value: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let sanitized_value = raw_value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();

    if prefix.is_empty() {
        sanitized_value
    } else {
        format!("{prefix}/{sanitized_value}")
    }
}

fn generate_openai_git_naming(
    config: &OpenAiGitNamingConfig,
    context: &GitNamingPromptContext<'_>,
) -> Result<ResolvedGitNaming, GitNamingError> {
    let prompt = render_prompt(&config.prompt_template, context);
    let api_url = format!("{}/responses", config.api_base_url.trim_end_matches('/'));
    let body = json!({
        "model": config.model,
        "input": prompt,
        "text": {
            "format": {
                "type": "json_object"
            }
        }
    });
    let raw = run_curl(
        &config.curl_bin,
        &api_url,
        &config.api_key,
        config.timeout_seconds,
        &body.to_string(),
    )?;
    let response_json: Value =
        serde_json::from_slice(&raw).map_err(|e| GitNamingError::InvalidResponse(e.to_string()))?;
    let response_text = extract_response_text(&response_json)?;
    let parsed: Value = serde_json::from_str(&response_text)
        .map_err(|e| GitNamingError::InvalidResponse(e.to_string()))?;
    let branch_name = parsed
        .get("branch_name")
        .and_then(Value::as_str)
        .and_then(normalize_generated_branch_name)
        .ok_or_else(|| {
            GitNamingError::InvalidResponse(
                "branch_name is missing or invalid in naming response".to_string(),
            )
        })?;
    let commit_message = parsed
        .get("commit_message")
        .and_then(Value::as_str)
        .and_then(normalize_commit_message)
        .ok_or_else(|| {
            GitNamingError::InvalidResponse(
                "commit_message is missing or invalid in naming response".to_string(),
            )
        })?;

    Ok(ResolvedGitNaming {
        branch_name,
        commit_message,
    })
}

fn render_prompt(template: &str, context: &GitNamingPromptContext<'_>) -> String {
    template
        .replace("{{job_id}}", context.job_id)
        .replace("{{repo_ref}}", context.repo_ref)
        .replace("{{repo_alias}}", context.repo_alias.unwrap_or(""))
        .replace("{{revision}}", context.revision)
        .replace("{{instruction}}", context.instruction)
        .replace("{{platform}}", context.platform)
}

fn extract_response_text(response_json: &Value) -> Result<String, GitNamingError> {
    if let Some(output_text) = response_json.get("output_text").and_then(Value::as_str) {
        let trimmed = output_text.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let mut chunks = Vec::new();
    for item in response_json
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for content in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    chunks.push(trimmed.to_string());
                }
            }
        }
    }

    if chunks.is_empty() {
        return Err(GitNamingError::InvalidResponse(
            "responses API output did not contain text".to_string(),
        ));
    }

    Ok(chunks.join("\n"))
}

fn normalize_generated_branch_name(raw: &str) -> Option<String> {
    let candidate = raw.trim().trim_matches('/');
    if candidate.is_empty() || candidate == "@" || candidate.contains("@{") {
        return None;
    }

    let mut normalized_segments = Vec::new();
    for raw_segment in candidate.split('/') {
        let mapped = raw_segment
            .trim()
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let segment = mapped.trim_matches('.');
        if segment.is_empty()
            || segment == "@"
            || segment == "."
            || segment == ".."
            || segment.ends_with(".lock")
            || segment.contains("..")
            || segment.contains("@{")
        {
            return None;
        }
        normalized_segments.push(segment.to_string());
    }

    let normalized = normalized_segments.join("/");
    if normalized.is_empty() || normalized.ends_with('/') {
        return None;
    }

    Some(normalized)
}

fn normalize_commit_message(raw: &str) -> Option<String> {
    if raw.contains('\n') || raw.contains('\r') {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn run_curl(
    curl_bin: &str,
    api_url: &str,
    api_key: &str,
    timeout_seconds: u64,
    body: &str,
) -> Result<Vec<u8>, GitNamingError> {
    let args = vec![
        "-sS".to_string(),
        "-X".to_string(),
        "POST".to_string(),
        api_url.to_string(),
        "-H".to_string(),
        format!("Authorization: Bearer {api_key}"),
        "-H".to_string(),
        "Content-Type: application/json".to_string(),
        "--max-time".to_string(),
        timeout_seconds.to_string(),
        "-d".to_string(),
        body.to_string(),
    ];
    let output = Command::new(curl_bin).args(&args).output()?;
    if !output.status.success() {
        return Err(GitNamingError::CommandFailed {
            program: curl_bin.to_string(),
            args,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};
    use tempfile::TempDir;

    #[test]
    fn build_legacy_git_naming_preserves_old_mechanism() {
        let resolved = build_legacy_git_naming("openoman", "job-123");
        assert_eq!(resolved.branch_name, "openoman/job-123");
        assert_eq!(resolved.commit_message, "OpenOMAN job job-123");
    }

    #[test]
    fn resolve_git_naming_falls_back_to_legacy_when_disabled() {
        let context = GitNamingPromptContext {
            job_id: "job-123",
            repo_ref: "https://github.com/acme/demo.git",
            repo_alias: Some("demo"),
            revision: "main",
            instruction: "update readme",
            platform: "github",
        };

        let resolved = resolve_git_naming(None, "topic", &context);
        assert_eq!(resolved.branch_name, "topic/job-123");
        assert_eq!(resolved.commit_message, "OpenOMAN job job-123");
    }

    #[test]
    fn resolve_git_naming_uses_openai_response_when_valid() {
        let temp = TempDir::new().expect("tempdir");
        let curl_bin = write_fake_curl(
            temp.path(),
            r#"{"output":[{"content":[{"text":"{\"branch_name\":\"feature/readme-refresh\",\"commit_message\":\"Refresh README copy\"}"}]}]}"#,
        );
        let config = OpenAiGitNamingConfig {
            api_base_url: "https://api.openai.test/v1".to_string(),
            model: "gpt-test".to_string(),
            api_key: "secret".to_string(),
            curl_bin: curl_bin.display().to_string(),
            timeout_seconds: 5,
            prompt_template: "job={{job_id}} instruction={{instruction}}".to_string(),
        };
        let context = GitNamingPromptContext {
            job_id: "job-123",
            repo_ref: "https://github.com/acme/demo.git",
            repo_alias: Some("demo"),
            revision: "main",
            instruction: "update readme",
            platform: "github",
        };

        let resolved = resolve_git_naming(Some(&config), "openoman", &context);
        assert_eq!(resolved.branch_name, "feature/readme-refresh");
        assert_eq!(resolved.commit_message, "Refresh README copy");
    }

    #[test]
    fn resolve_git_naming_falls_back_when_response_is_invalid() {
        let temp = TempDir::new().expect("tempdir");
        let curl_bin = write_fake_curl(
            temp.path(),
            r#"{"output":[{"content":[{"text":"{\"branch_name\":\"/\",\"commit_message\":\"\"}"}]}]}"#,
        );
        let config = OpenAiGitNamingConfig {
            api_base_url: "https://api.openai.test/v1".to_string(),
            model: "gpt-test".to_string(),
            api_key: "secret".to_string(),
            curl_bin: curl_bin.display().to_string(),
            timeout_seconds: 5,
            prompt_template: "job={{job_id}}".to_string(),
        };
        let context = GitNamingPromptContext {
            job_id: "job-123",
            repo_ref: "https://github.com/acme/demo.git",
            repo_alias: Some("demo"),
            revision: "main",
            instruction: "update readme",
            platform: "github",
        };

        let resolved = resolve_git_naming(Some(&config), "openoman", &context);
        assert_eq!(resolved.branch_name, "openoman/job-123");
        assert_eq!(resolved.commit_message, "OpenOMAN job job-123");
    }

    fn write_fake_curl(root: &Path, response_body: &str) -> std::path::PathBuf {
        let script_path = root.join("fake-curl.sh");
        fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf '%s' '{}'\n",
                response_body.replace('\'', "'\\''")
            ),
        )
        .expect("write fake curl");
        fs::set_permissions(&script_path, PermissionsExt::from_mode(0o755))
            .expect("chmod fake curl");
        script_path
    }
}
