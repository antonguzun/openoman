use std::{
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
        let workspace_root = self.trusted_workspace_root.join(workspace_id);
        let trusted_clone_dir = workspace_root.join("trusted-clone");
        let sandbox_workspace_dir = workspace_root.join("sandbox-workspace");

        if workspace_root.exists() {
            fs::remove_dir_all(&workspace_root)?;
        }
        fs::create_dir_all(&workspace_root)?;

        run_git(
            None,
            vec![
                OsStr::new("clone"),
                OsStr::new("--quiet"),
                OsStr::new(repo_ref.as_str()),
                trusted_clone_dir.as_os_str(),
            ],
        )?;
        run_git(
            Some(&trusted_clone_dir),
            vec![
                OsStr::new("checkout"),
                OsStr::new("--quiet"),
                OsStr::new(revision.as_str()),
            ],
        )?;

        copy_tree(&trusted_clone_dir, &sandbox_workspace_dir, &[])?;
        sanitize_sandbox_git(&sandbox_workspace_dir)?;

        Ok(PreparedWorkspace {
            trusted_clone_dir,
            sandbox_workspace_dir,
        })
    }
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

    let mut command = Command::new("git");
    command.args(&collected_args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }

    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(GitError::CommandFailed {
        program: "git".to_string(),
        args: collected_args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_git_stdout(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<Vec<u8>, GitError> {
    let collected_args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();

    let mut command = Command::new("git");
    command.args(&collected_args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }

    let output = command.output()?;
    if output.status.success() {
        return Ok(output.stdout);
    }

    Err(GitError::CommandFailed {
        program: "git".to_string(),
        args: collected_args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
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
