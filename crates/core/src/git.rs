use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::domain::job::{RepoRef, Revision};

#[derive(Debug)]
pub enum GitError {
    Io(std::io::Error),
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
}

impl std::fmt::Display for GitError {
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
        }
    }
}

impl std::error::Error for GitError {}

impl From<std::io::Error> for GitError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone)]
pub struct PreparedWorkspace {
    pub trusted_clone_dir: PathBuf,
    pub sandbox_workspace_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PrepareWorkspaceOptions<'a> {
    pub env_overlay_dir: Option<&'a Path>,
    pub clone_token: Option<&'a str>,
    pub post_clone_command: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct GitAdapter {
    trusted_workspace_root: PathBuf,
}

impl GitAdapter {
    pub fn new(trusted_workspace_root: impl AsRef<Path>) -> Self {
        Self {
            trusted_workspace_root: trusted_workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn prepare_workspace(
        &self,
        repo_ref: &RepoRef,
        revision: &Revision,
        workspace_id: &str,
    ) -> Result<PreparedWorkspace, GitError> {
        self.prepare_workspace_with_options(
            repo_ref,
            revision,
            workspace_id,
            PrepareWorkspaceOptions::default(),
        )
    }

    pub fn prepare_workspace_with_env_overlay(
        &self,
        repo_ref: &RepoRef,
        revision: &Revision,
        workspace_id: &str,
        env_overlay_dir: Option<&Path>,
    ) -> Result<PreparedWorkspace, GitError> {
        self.prepare_workspace_with_options(
            repo_ref,
            revision,
            workspace_id,
            PrepareWorkspaceOptions {
                env_overlay_dir,
                ..PrepareWorkspaceOptions::default()
            },
        )
    }

    pub fn prepare_workspace_with_options(
        &self,
        repo_ref: &RepoRef,
        revision: &Revision,
        workspace_id: &str,
        options: PrepareWorkspaceOptions<'_>,
    ) -> Result<PreparedWorkspace, GitError> {
        let workspace_root = self.trusted_workspace_root.join(workspace_id);
        let trusted_clone_dir = workspace_root.join("trusted-clone");
        let sandbox_workspace_dir = workspace_root.join("sandbox-workspace");

        if workspace_root.exists() {
            fs::remove_dir_all(&workspace_root)?;
        }
        fs::create_dir_all(&workspace_root)?;

        run_git_clone(repo_ref.as_str(), &trusted_clone_dir, options.clone_token)?;
        run_git(
            Some(&trusted_clone_dir),
            vec![
                OsStr::new("checkout"),
                OsStr::new("--quiet"),
                OsStr::new(revision.as_str()),
            ],
        )?;
        run_post_clone_command(&trusted_clone_dir, options.post_clone_command)?;

        copy_tree(&trusted_clone_dir, &sandbox_workspace_dir, &[])?;
        let injected_files =
            copy_env_overlay_files(options.env_overlay_dir, &sandbox_workspace_dir)?;
        if !injected_files.is_empty() {
            append_git_exclude_entries(&trusted_clone_dir, &injected_files)?;
            append_git_exclude_entries(&sandbox_workspace_dir, &injected_files)?;
        }
        sanitize_sandbox_git(&sandbox_workspace_dir)?;

        Ok(PreparedWorkspace {
            trusted_clone_dir,
            sandbox_workspace_dir,
        })
    }
}

fn run_post_clone_command(
    trusted_clone_dir: &Path,
    post_clone_command: Option<&str>,
) -> Result<(), GitError> {
    let Some(post_clone_command) = post_clone_command
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let output = Command::new("/bin/sh")
        .current_dir(trusted_clone_dir)
        .args(["-lc", post_clone_command])
        .output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(GitError::CommandFailed {
        program: "/bin/sh".to_string(),
        args: vec!["-lc".to_string(), post_clone_command.to_string()],
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_git_clone(
    repo_ref: &str,
    trusted_clone_dir: &Path,
    clone_token: Option<&str>,
) -> Result<(), GitError> {
    let mut args = Vec::new();
    if repo_ref.starts_with("https://") {
        if let Some(token) = clone_token.map(str::trim).filter(|value| !value.is_empty()) {
            args.push(OsString::from("-c"));
            args.push(OsString::from(format!(
                "http.extraheader={}",
                build_git_http_auth_header(token)
            )));
            args.push(OsString::from("-c"));
            args.push(OsString::from("credential.helper="));
        }
    }

    args.push(OsString::from("clone"));
    args.push(OsString::from("--quiet"));
    args.push(OsString::from(repo_ref));
    args.push(trusted_clone_dir.as_os_str().to_os_string());
    run_git_os(None, &args)
}

fn copy_env_overlay_files(
    env_overlay_dir: Option<&Path>,
    sandbox_workspace_dir: &Path,
) -> Result<Vec<String>, GitError> {
    let Some(env_overlay_dir) = env_overlay_dir else {
        return Ok(Vec::new());
    };
    if !env_overlay_dir.exists() {
        return Ok(Vec::new());
    }
    if !env_overlay_dir.is_dir() {
        return Err(GitError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "env overlay path is not a directory: {}",
                env_overlay_dir.display()
            ),
        )));
    }

    let mut injected = Vec::new();
    for entry in fs::read_dir(env_overlay_dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let destination = sandbox_workspace_dir.join(&file_name);
        fs::copy(entry.path(), destination)?;
        injected.push(file_name.to_string_lossy().to_string());
    }
    injected.sort();
    injected.dedup();
    Ok(injected)
}

fn append_git_exclude_entries(repo_dir: &Path, file_names: &[String]) -> Result<(), GitError> {
    let exclude_path = repo_dir.join(".git").join("info").join("exclude");
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = if exclude_path.exists() {
        fs::read_to_string(&exclude_path)?
    } else {
        String::new()
    };
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let mut known: HashSet<String> = lines.iter().cloned().collect();
    for file_name in file_names {
        let entry = format!("/{}", file_name);
        if known.insert(entry.clone()) {
            lines.push(entry);
        }
    }
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(exclude_path, content)?;
    Ok(())
}

pub fn write_canonical_patch(
    trusted_clone_dir: &Path,
    modified_workspace_dir: &Path,
    output_path: &Path,
) -> Result<(), GitError> {
    let temp_workspace = unique_temp_workspace("openoman-patch");
    if temp_workspace.exists() {
        fs::remove_dir_all(&temp_workspace)?;
    }
    fs::create_dir_all(&temp_workspace)?;

    let result: Result<(), GitError> = (|| {
        copy_tree(trusted_clone_dir, &temp_workspace, &[])?;
        clear_worktree_except_git(&temp_workspace)?;
        copy_tree(modified_workspace_dir, &temp_workspace, &[".git"])?;

        run_git(
            Some(&temp_workspace),
            vec![OsStr::new("add"), OsStr::new("-A"), OsStr::new(".")],
        )?;
        let diff = run_git_stdout(
            Some(&temp_workspace),
            vec![
                OsStr::new("diff"),
                OsStr::new("--cached"),
                OsStr::new("HEAD"),
                OsStr::new("--binary"),
                OsStr::new("--full-index"),
                OsStr::new("--no-ext-diff"),
                OsStr::new("--no-color"),
                OsStr::new("--no-renames"),
            ],
        )?;

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output_path, diff)?;
        Ok(())
    })();

    let cleanup_result = if temp_workspace.exists() {
        fs::remove_dir_all(&temp_workspace)
    } else {
        Ok(())
    };

    match (result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), Ok(())) => Err(err),
        (Ok(()), Err(err)) => Err(err.into()),
        (Err(err), Err(_)) => Err(err),
    }
}

fn unique_temp_workspace(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn sanitize_sandbox_git(workspace_dir: &Path) -> Result<(), GitError> {
    let git_dir = workspace_dir.join(".git");
    let config_path = git_dir.join("config");
    if config_path.exists() {
        let raw = fs::read_to_string(&config_path)?;
        fs::write(&config_path, sanitize_git_config(&raw))?;
    }

    let hooks_dir = git_dir.join("hooks");
    if hooks_dir.exists() {
        fs::remove_dir_all(&hooks_dir)?;
    }
    fs::create_dir_all(&hooks_dir)?;

    let alternates = git_dir.join("objects").join("info").join("alternates");
    if alternates.exists() {
        fs::remove_file(alternates)?;
    }

    run_git(
        Some(workspace_dir),
        vec![
            OsStr::new("config"),
            OsStr::new("user.name"),
            OsStr::new("OpenOMAN Sandbox"),
        ],
    )?;
    run_git(
        Some(workspace_dir),
        vec![
            OsStr::new("config"),
            OsStr::new("user.email"),
            OsStr::new("sandbox@openoman.invalid"),
        ],
    )?;

    Ok(())
}

fn sanitize_git_config(raw: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SectionMode {
        Keep,
        Drop,
        Core,
    }

    let mut mode = SectionMode::Keep;
    let mut out = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let header = trimmed.to_ascii_lowercase();
            mode = if header == "[core]" {
                SectionMode::Core
            } else if header.starts_with("[remote ")
                || header.starts_with("[branch ")
                || header == "[credential]"
                || header.starts_with("[credential ")
                || header == "[http]"
                || header.starts_with("[http ")
                || header == "[url]"
                || header.starts_with("[url ")
                || header == "[include]"
                || header.starts_with("[include ")
                || header == "[includeif]"
                || header.starts_with("[includeif ")
            {
                SectionMode::Drop
            } else {
                SectionMode::Keep
            };

            if mode != SectionMode::Drop {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }

        if mode == SectionMode::Drop {
            continue;
        }

        if mode == SectionMode::Core && is_git_config_key(trimmed, "sshcommand") {
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

fn is_git_config_key(line: &str, expected_key: &str) -> bool {
    line.split_once('=')
        .map(|(key, _)| key.trim().eq_ignore_ascii_case(expected_key))
        .unwrap_or(false)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    exclude_root_entries: &[&str],
) -> Result<(), GitError> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name_text = file_name.to_string_lossy();

        if exclude_root_entries
            .iter()
            .any(|excluded| *excluded == file_name_text)
        {
            continue;
        }

        let target_path = destination.join(&file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&path, &target_path, &[])?;
        } else if file_type.is_file() {
            fs::copy(&path, &target_path)?;
        }
    }

    Ok(())
}

fn clear_worktree_except_git(path: &Path) -> Result<(), GitError> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() == OsStr::new(".git") {
            continue;
        }

        let entry_path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            fs::remove_dir_all(entry_path)?;
        } else {
            fs::remove_file(entry_path)?;
        }
    }

    Ok(())
}

fn run_git(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<(), GitError> {
    let collected_args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    run_git_os(current_dir, &collected_args)
}

fn run_git_stdout(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<Vec<u8>, GitError> {
    let collected_args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    run_git_stdout_os(current_dir, &collected_args)
}

fn run_git_os(current_dir: Option<&Path>, args: &[OsString]) -> Result<(), GitError> {
    let output = run_git_command(current_dir, args)?;
    if output.status.success() {
        return Ok(());
    }

    Err(GitError::CommandFailed {
        program: "git".to_string(),
        args: render_git_args_for_error(args),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_git_stdout_os(current_dir: Option<&Path>, args: &[OsString]) -> Result<Vec<u8>, GitError> {
    let output = run_git_command(current_dir, args)?;
    if output.status.success() {
        return Ok(output.stdout);
    }

    Err(GitError::CommandFailed {
        program: "git".to_string(),
        args: render_git_args_for_error(args),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_git_command(
    current_dir: Option<&Path>,
    args: &[OsString],
) -> Result<std::process::Output, GitError> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    Ok(command.output()?)
}

fn render_git_args_for_error(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| {
            let text = arg.to_string_lossy();
            if text.starts_with("http.extraheader=") {
                "http.extraheader=<redacted>".to_string()
            } else {
                text.to_string()
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prepares_workspace_creates_sanitized_git_repo_for_sandbox() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new("main").expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));

        let prepared = adapter
            .prepare_workspace(&repo_ref, &revision, "job-001")
            .expect("prepare workspace");

        let trusted_file = fs::read_to_string(prepared.trusted_clone_dir.join("README.md"))
            .expect("trusted file exists");
        assert_eq!(trusted_file.trim(), "main branch content");
        assert!(prepared.trusted_clone_dir.join(".git").exists());

        let sandbox_file = fs::read_to_string(prepared.sandbox_workspace_dir.join("README.md"))
            .expect("sandbox file exists");
        assert_eq!(sandbox_file.trim(), "main branch content");
        assert!(prepared.sandbox_workspace_dir.join(".git").exists());

        let config = fs::read_to_string(prepared.sandbox_workspace_dir.join(".git").join("config"))
            .expect("sanitized config");
        assert!(!config.contains("[remote \"origin\"]"));
        assert!(!config.contains("url = "));
        assert!(config.contains("name = OpenOMAN Sandbox"));
        assert!(config.contains("email = sandbox@openoman.invalid"));

        let hooks_dir = prepared.sandbox_workspace_dir.join(".git").join("hooks");
        let hooks_entries = fs::read_dir(hooks_dir)
            .expect("hooks dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("hooks entries");
        assert!(hooks_entries.is_empty());
    }

    #[test]
    fn supports_checkout_by_commit_sha() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);

        let first_sha = initial_commit_sha(&fixture_repo);

        fs::write(fixture_repo.join("README.md"), "second commit\n").expect("write second commit");
        git(&fixture_repo, ["add", "README.md"]);
        git(&fixture_repo, ["commit", "-m", "second"]);

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new(first_sha).expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));

        let prepared = adapter
            .prepare_workspace(&repo_ref, &revision, "job-002")
            .expect("prepare workspace");

        let sandbox_file = fs::read_to_string(prepared.sandbox_workspace_dir.join("README.md"))
            .expect("sandbox file exists");
        assert_eq!(sandbox_file.trim(), "initial content");
    }

    #[test]
    fn write_canonical_patch_captures_modify_add_delete() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new("main").expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));
        let prepared = adapter
            .prepare_workspace(&repo_ref, &revision, "job-003")
            .expect("prepare workspace");

        fs::write(
            prepared.sandbox_workspace_dir.join("README.md"),
            "main branch content\n\n",
        )
        .expect("modify readme");
        fs::remove_file(prepared.sandbox_workspace_dir.join("notes.txt")).expect("remove notes");
        fs::write(
            prepared.sandbox_workspace_dir.join("NEW_FILE.md"),
            "new content\n",
        )
        .expect("add file");

        let patch_one = temp.path().join("patch-1.diff");
        let patch_two = temp.path().join("patch-2.diff");

        write_canonical_patch(
            &prepared.trusted_clone_dir,
            &prepared.sandbox_workspace_dir,
            &patch_one,
        )
        .expect("write patch");
        write_canonical_patch(
            &prepared.trusted_clone_dir,
            &prepared.sandbox_workspace_dir,
            &patch_two,
        )
        .expect("write second patch");

        let patch_one_contents = fs::read(&patch_one).expect("patch contents");
        let patch_two_contents = fs::read(&patch_two).expect("patch contents");
        assert_eq!(patch_one_contents, patch_two_contents);

        let patch_text = String::from_utf8_lossy(&patch_one_contents);
        assert!(patch_text.contains("README.md"));
        assert!(patch_text.contains("notes.txt"));
        assert!(patch_text.contains("NEW_FILE.md"));
    }

    #[test]
    fn env_overlay_files_are_staged_for_sandbox_and_excluded_from_patch() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        let env_overlay_dir = temp.path().join("env_for_repo/demo");
        init_fixture_repo(&fixture_repo);
        fs::create_dir_all(env_overlay_dir.join("nested")).expect("create env overlay dir");
        fs::write(env_overlay_dir.join(".env"), "API_TOKEN=demo\n").expect("write .env");
        fs::write(env_overlay_dir.join(".env.test"), "VALUE=1\n").expect("write .env.test");
        fs::write(env_overlay_dir.join("nested/ignored.env"), "IGNORED=1\n")
            .expect("write nested env");

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new("main").expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));
        let prepared = adapter
            .prepare_workspace_with_env_overlay(
                &repo_ref,
                &revision,
                "job-004",
                Some(&env_overlay_dir),
            )
            .expect("prepare workspace");

        assert!(prepared.sandbox_workspace_dir.join(".env").exists());
        assert!(prepared.sandbox_workspace_dir.join(".env.test").exists());
        assert!(!prepared
            .sandbox_workspace_dir
            .join("nested/ignored.env")
            .exists());

        let patch_path = temp.path().join("patch-overlay.diff");
        write_canonical_patch(
            &prepared.trusted_clone_dir,
            &prepared.sandbox_workspace_dir,
            &patch_path,
        )
        .expect("write patch");
        let patch_text = fs::read_to_string(&patch_path).expect("patch text");
        assert!(patch_text.trim().is_empty());

        let trusted_exclude = fs::read_to_string(
            prepared
                .trusted_clone_dir
                .join(".git")
                .join("info")
                .join("exclude"),
        )
        .expect("trusted exclude");
        assert!(trusted_exclude.contains("/.env"));
        assert!(trusted_exclude.contains("/.env.test"));
    }

    #[test]
    fn post_clone_command_runs_in_trusted_clone_before_sandbox_copy() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new("main").expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));
        let prepared = adapter
            .prepare_workspace_with_options(
                &repo_ref,
                &revision,
                "job-005",
                PrepareWorkspaceOptions {
                    post_clone_command: Some(
                        "mkdir -p generated && printf 'from hook\\n' > generated/post-clone.txt",
                    ),
                    ..PrepareWorkspaceOptions::default()
                },
            )
            .expect("prepare workspace");

        let trusted_generated = fs::read_to_string(
            prepared
                .trusted_clone_dir
                .join("generated")
                .join("post-clone.txt"),
        )
        .expect("trusted generated file");
        let sandbox_generated = fs::read_to_string(
            prepared
                .sandbox_workspace_dir
                .join("generated")
                .join("post-clone.txt"),
        )
        .expect("sandbox generated file");

        assert_eq!(trusted_generated, "from hook\n");
        assert_eq!(sandbox_generated, "from hook\n");
    }

    #[test]
    fn failing_post_clone_command_aborts_workspace_preparation() {
        let temp = TempDir::new().expect("tempdir");
        let fixture_repo = temp.path().join("fixture-repo");
        init_fixture_repo(&fixture_repo);

        let repo_ref = RepoRef::new(fixture_repo.display().to_string()).expect("repo ref");
        let revision = Revision::new("main").expect("revision");
        let adapter = GitAdapter::new(temp.path().join("workspaces"));
        let err = adapter
            .prepare_workspace_with_options(
                &repo_ref,
                &revision,
                "job-006",
                PrepareWorkspaceOptions {
                    post_clone_command: Some("echo post-clone failed >&2; exit 17"),
                    ..PrepareWorkspaceOptions::default()
                },
            )
            .expect_err("post clone command should fail");

        match err {
            GitError::CommandFailed {
                program,
                args,
                stderr,
            } => {
                assert_eq!(program, "/bin/sh");
                assert_eq!(
                    args,
                    vec![
                        "-lc".to_string(),
                        "echo post-clone failed >&2; exit 17".to_string()
                    ]
                );
                assert!(stderr.contains("post-clone failed"));
            }
            other => panic!("unexpected error: {other}"),
        }
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

    fn initial_commit_sha(path: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("rev-list")
            .arg("--max-parents=0")
            .arg("HEAD")
            .output()
            .expect("rev-parse output");
        assert!(output.status.success(), "rev-parse failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
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
}
