use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::Command,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct GitHubPublisherConfig {
    pub api_base_url: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub base_branch: String,
    pub branch_prefix: String,
    pub push_url: String,
    pub token: String,
    pub curl_bin: String,
    pub git_user_name: String,
    pub git_user_email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPullRequest {
    pub branch_name: String,
    pub pull_request_url: String,
    pub pull_request_number: u64,
}

#[derive(Debug, Clone)]
pub struct GitHubPublisher {
    config: GitHubPublisherConfig,
}

impl GitHubPublisher {
    pub fn new(config: GitHubPublisherConfig) -> Self {
        Self { config }
    }

    pub fn publish_patch(
        &self,
        branch_name: &str,
        commit_message: &str,
        instruction: &str,
        trusted_clone_dir: &Path,
        patch_path: &Path,
    ) -> Result<PublishedPullRequest, GitHubPublishError> {
        run_git(
            Some(trusted_clone_dir),
            vec![
                OsStr::new("checkout"),
                OsStr::new("-B"),
                OsStr::new(branch_name),
            ],
        )?;
        run_git(
            Some(trusted_clone_dir),
            vec![
                OsStr::new("apply"),
                OsStr::new("--binary"),
                patch_path.as_os_str(),
            ],
        )?;
        run_git(
            Some(trusted_clone_dir),
            vec![
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new(&self.config.git_user_name),
            ],
        )?;
        run_git(
            Some(trusted_clone_dir),
            vec![
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new(&self.config.git_user_email),
            ],
        )?;
        if !worktree_has_changes(trusted_clone_dir)? {
            return Err(GitHubPublishError::InvalidResponse(
                "canonical patch produced no tracked changes".to_string(),
            ));
        }
        run_git(
            Some(trusted_clone_dir),
            vec![OsStr::new("add"), OsStr::new("-A"), OsStr::new(".")],
        )?;
        run_git(
            Some(trusted_clone_dir),
            vec![
                OsStr::new("commit"),
                OsStr::new("--quiet"),
                OsStr::new("-m"),
                OsStr::new(commit_message),
            ],
        )?;
        push_branch(
            trusted_clone_dir,
            &self.config.push_url,
            branch_name,
            &self.config.token,
        )?;

        let created = self.create_pull_request(branch_name, commit_message, instruction)?;
        Ok(PublishedPullRequest {
            branch_name: branch_name.to_string(),
            pull_request_url: created.html_url,
            pull_request_number: created.number,
        })
    }

    fn create_pull_request(
        &self,
        branch_name: &str,
        commit_message: &str,
        instruction: &str,
    ) -> Result<CreatePullRequestResponse, GitHubPublishError> {
        let api_url = format!(
            "{}/repos/{}/{}/pulls",
            self.config.api_base_url.trim_end_matches('/'),
            self.config.repo_owner,
            self.config.repo_name
        );
        let payload = CreatePullRequestRequest {
            title: commit_message.to_string(),
            body: format!("Instruction:\n\n{instruction}"),
            head: branch_name.to_string(),
            base: self.config.base_branch.clone(),
        };
        let body = serde_json::to_string(&payload)
            .map_err(|e| GitHubPublishError::InvalidResponse(e.to_string()))?;
        let raw = run_curl(&self.config.curl_bin, &api_url, &self.config.token, &body)?;
        let (response_body, status_code) = split_curl_response(&raw)?;
        if !(200..300).contains(&status_code) {
            return Err(GitHubPublishError::ApiError {
                status_code,
                body: response_body,
            });
        }

        serde_json::from_str(&response_body)
            .map_err(|e| GitHubPublishError::InvalidResponse(e.to_string()))
    }
}

#[derive(Debug, Serialize)]
struct CreatePullRequestRequest {
    title: String,
    body: String,
    head: String,
    base: String,
}

#[derive(Debug, Deserialize)]
struct CreatePullRequestResponse {
    number: u64,
    html_url: String,
}

#[derive(Debug)]
pub enum GitHubPublishError {
    Io(std::io::Error),
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    ApiError {
        status_code: u16,
        body: String,
    },
    InvalidResponse(String),
}

impl std::fmt::Display for GitHubPublishError {
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
            Self::ApiError { status_code, body } => {
                write!(
                    f,
                    "github api request failed with status {status_code}: {body}"
                )
            }
            Self::InvalidResponse(message) => write!(f, "invalid github response: {message}"),
        }
    }
}

impl std::error::Error for GitHubPublishError {}

impl From<std::io::Error> for GitHubPublishError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

fn worktree_has_changes(current_dir: &Path) -> Result<bool, GitHubPublishError> {
    let output = run_git_stdout(
        Some(current_dir),
        vec![OsStr::new("status"), OsStr::new("--porcelain")],
    )?;
    Ok(!String::from_utf8_lossy(&output).trim().is_empty())
}

fn push_branch(
    current_dir: &Path,
    push_url: &str,
    branch_name: &str,
    token: &str,
) -> Result<(), GitHubPublishError> {
    let mut args = Vec::new();
    if push_url.starts_with("https://") && !token.is_empty() {
        args.push(OsString::from("-c"));
        args.push(OsString::from(format!(
            "http.extraheader={}",
            build_git_http_auth_header(token)
        )));
        args.push(OsString::from("-c"));
        args.push(OsString::from("credential.helper="));
    }
    args.push(OsString::from("push"));
    args.push(OsString::from(push_url));
    args.push(OsString::from(format!("HEAD:refs/heads/{branch_name}")));
    run_git_os(Some(current_dir), &args)
}

fn build_git_http_auth_header(token: &str) -> String {
    let credentials = format!("x-access-token:{token}");
    format!(
        "Authorization: Basic {}",
        encode_base64(credentials.as_bytes())
    )
}

fn encode_base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);

        output.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }

    output
}

fn run_curl(
    curl_bin: &str,
    api_url: &str,
    token: &str,
    body: &str,
) -> Result<Vec<u8>, GitHubPublishError> {
    let args = [
        "--silent",
        "--show-error",
        "--location",
        "--request",
        "POST",
        "--header",
        "Accept: application/vnd.github+json",
        "--header",
        "Content-Type: application/json",
        "--header",
        "User-Agent: openoman/0.1",
        "--header",
        "X-GitHub-Api-Version: 2022-11-28",
        "--header",
        &format!("Authorization: Bearer {token}"),
        "--data",
        body,
        "--write-out",
        "\n%{http_code}",
        api_url,
    ];

    let output = Command::new(curl_bin).args(args).output()?;
    if output.status.success() {
        return Ok(output.stdout);
    }

    Err(GitHubPublishError::CommandFailed {
        program: curl_bin.to_string(),
        args: args.iter().map(ToString::to_string).collect(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn split_curl_response(raw: &[u8]) -> Result<(String, u16), GitHubPublishError> {
    let output = String::from_utf8_lossy(raw);
    let Some((body, status)) = output.rsplit_once('\n') else {
        return Err(GitHubPublishError::InvalidResponse(
            "curl output did not contain an HTTP status code".to_string(),
        ));
    };
    let status_code = status
        .trim()
        .parse::<u16>()
        .map_err(|e| GitHubPublishError::InvalidResponse(e.to_string()))?;
    Ok((body.to_string(), status_code))
}

fn run_git(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<(), GitHubPublishError> {
    let collected_args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    run_git_os(current_dir, &collected_args)
}

fn run_git_os(current_dir: Option<&Path>, args: &[OsString]) -> Result<(), GitHubPublishError> {
    let output = run_command("git", current_dir, args)?;
    if output.status.success() {
        return Ok(());
    }

    Err(GitHubPublishError::CommandFailed {
        program: "git".to_string(),
        args: args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_git_stdout(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<Vec<u8>, GitHubPublishError> {
    let collected_args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let output = run_command("git", current_dir, &collected_args)?;
    if output.status.success() {
        return Ok(output.stdout);
    }

    Err(GitHubPublishError::CommandFailed {
        program: "git".to_string(),
        args: collected_args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_command(
    program: &str,
    current_dir: Option<&Path>,
    args: &[OsString],
) -> Result<std::process::Output, GitHubPublishError> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    if program == "git" {
        command.env("GIT_TERMINAL_PROMPT", "0");
    }
    Ok(command.output()?)
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt, path::Path};

    use tempfile::TempDir;

    use crate::{
        domain::job::{RepoRef, Revision},
        git::{write_canonical_patch, GitAdapter},
    };

    use super::*;

    #[test]
    fn git_https_auth_header_uses_basic_auth_for_pat() {
        assert_eq!(
            build_git_http_auth_header("test-token"),
            "Authorization: Basic eC1hY2Nlc3MtdG9rZW46dGVzdC10b2tlbg=="
        );
    }

    #[test]
    fn publish_patch_pushes_branch_and_creates_pull_request() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        let bare_remote = temp.path().join("remote.git");
        let curl_log = temp.path().join("curl.log");
        let curl_body = temp.path().join("curl-body.json");
        init_fixture_repo(&fixture_repo);
        git(
            temp.path(),
            [
                OsStr::new("clone"),
                OsStr::new("--bare"),
                fixture_repo.as_os_str(),
                bare_remote.as_os_str(),
            ],
        );

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new("main").expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));
        let prepared = adapter
            .prepare_workspace(&repo_ref, &revision, "job-publish")
            .expect("prepare workspace");
        fs::write(
            prepared.sandbox_workspace_dir.join("README.md"),
            "main branch content\n\npublished by openoman\n",
        )
        .expect("modify readme");
        let patch_path = temp.path().join("patch.diff");
        write_canonical_patch(
            &prepared.trusted_clone_dir,
            &prepared.sandbox_workspace_dir,
            &patch_path,
        )
        .expect("write patch");

        let curl_bin = write_fake_curl(temp.path(), &curl_log, &curl_body);
        let publisher = GitHubPublisher::new(GitHubPublisherConfig {
            api_base_url: "https://api.example.test".to_string(),
            repo_owner: "acme".to_string(),
            repo_name: "demo".to_string(),
            base_branch: "main".to_string(),
            branch_prefix: "openoman".to_string(),
            push_url: bare_remote.display().to_string(),
            token: "test-token".to_string(),
            curl_bin: curl_bin.display().to_string(),
            git_user_name: "Repo Bot".to_string(),
            git_user_email: "repo-bot@example.test".to_string(),
        });

        let published = publisher
            .publish_patch(
                "openoman/job-publish",
                "Refresh README copy",
                "append a line to README",
                &prepared.trusted_clone_dir,
                &patch_path,
            )
            .expect("publish should succeed");

        assert_eq!(
            published,
            PublishedPullRequest {
                branch_name: "openoman/job-publish".to_string(),
                pull_request_url: "https://example.test/pulls/17".to_string(),
                pull_request_number: 17,
            }
        );

        let remote_head = git_stdout(
            temp.path(),
            [
                OsStr::new("--git-dir"),
                bare_remote.as_os_str(),
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new("refs/heads/openoman/job-publish"),
            ],
        );
        assert!(!remote_head.trim().is_empty());
        let author = git_stdout(
            temp.path(),
            [
                OsStr::new("--git-dir"),
                bare_remote.as_os_str(),
                OsStr::new("show"),
                OsStr::new("-s"),
                OsStr::new("--format=%an <%ae>"),
                OsStr::new("refs/heads/openoman/job-publish"),
            ],
        );
        assert_eq!(author, "Repo Bot <repo-bot@example.test>");

        let curl_invocation = fs::read_to_string(&curl_log).expect("curl log");
        let curl_request_body = fs::read_to_string(&curl_body).expect("curl body");
        assert!(curl_invocation.contains("Authorization: Bearer test-token"));
        assert!(curl_invocation.contains("https://api.example.test/repos/acme/demo/pulls"));
        assert!(curl_request_body.contains("\"title\":\"Refresh README copy\""));
        assert!(curl_request_body.contains("\"head\":\"openoman/job-publish\""));
        assert!(curl_request_body.contains("\"base\":\"main\""));
    }

    fn write_fake_curl(root: &Path, log_path: &Path, body_path: &Path) -> std::path::PathBuf {
        let script_path = root.join("fake-curl.sh");
        fs::write(
            &script_path,
            format!(
                "#!/usr/bin/env sh\nset -eu\nlog_path='{}'\nbody_path='{}'\n: > \"$log_path\"\n: > \"$body_path\"\nwhile [ \"$#\" -gt 0 ]; do\n  printf '%s\\n' \"$1\" >> \"$log_path\"\n  case \"$1\" in\n    --data)\n      printf '%s' \"$2\" > \"$body_path\"\n      shift 2\n      ;;\n    *)\n      shift 1\n      ;;\n  esac\ndone\nprintf '{{\"number\":17,\"html_url\":\"https://example.test/pulls/17\"}}\\n201'\n",
                log_path.display(),
                body_path.display(),
            ),
        )
        .expect("write fake curl");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .expect("chmod fake curl");
        script_path
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
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .expect("git status");
        assert!(status.success(), "git command failed");
    }

    fn git_stdout(path: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git output");
        assert!(output.status.success(), "git command failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
