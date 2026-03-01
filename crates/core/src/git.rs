use std::{
    ffi::OsStr,
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

        copy_dir_without_git(&trusted_clone_dir, &sandbox_workspace_dir)?;

        Ok(PreparedWorkspace {
            trusted_clone_dir,
            sandbox_workspace_dir,
        })
    }
}

fn copy_dir_without_git(source: &Path, destination: &Path) -> Result<(), GitError> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();

        if file_name == ".git" {
            continue;
        }

        let target_path = destination.join(&file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_without_git(&path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(&path, &target_path)?;
        }
    }

    Ok(())
}

fn run_git(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<(), GitError> {
    let collected_args: Vec<_> = args
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

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let args = collected_args
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect();

    Err(GitError::CommandFailed {
        program: "git".to_string(),
        args,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prepares_workspace_and_excludes_git_from_sandbox_copy() {
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
        assert!(!prepared.sandbox_workspace_dir.join(".git").exists());
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
