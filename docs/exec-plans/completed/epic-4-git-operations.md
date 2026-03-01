# Epic 4 Git Operations (Trusted Clone/Workspaces)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the core library can clone a repository into a trusted host-side workspace, check out a requested revision (branch name or commit SHA), and create a sandbox workspace copy that excludes `.git` metadata. This gives later sandbox epics a deterministic source tree while reducing credential-leak risk into untrusted environments.

## Progress

- [x] (2026-03-01 00:00Z) Reviewed Epic 4 requirements in `MVP_DECOMPOSITION.md` and existing core/domain layout.
- [x] (2026-03-01 00:10Z) Added `crate::git::GitAdapter` with deterministic workspace pathing and clone/checkout workflow.
- [x] (2026-03-01 00:18Z) Implemented sandbox copy logic that excludes `.git` from exported workspace.
- [x] (2026-03-01 00:24Z) Added fixture-based unit tests for branch checkout and commit-SHA checkout correctness.
- [x] (2026-03-01 00:30Z) Ran format/lint/test/build checks and updated project checkpoints/docs.

## Surprises & Discoveries

- Observation: Generic `AsRef<OsStr>` arrays caused ambiguous type inference for checkout argument construction.
  Evidence: Rust compilation failed with E0283 until explicit `OsStr::new(...)` vectors were used.
- Observation: Capturing `HEAD` after fixture setup pointed at the second commit, not the initial commit needed for SHA checkout testing.
  Evidence: Test initially expected `initial content` but received `main branch content` until switching to `git rev-list --max-parents=0 HEAD`.

## Decision Log

- Decision: Use the system `git` CLI through `std::process::Command` rather than introducing a Rust git library.
  Rationale: Keeps dependencies light for MVP and mirrors real-world git behavior expected by operators.
  Date/Author: 2026-03-01 / Codex
- Decision: Remove existing workspace directory before each prepare call.
  Rationale: Guarantees deterministic outputs for repeated job IDs and avoids stale-file contamination.
  Date/Author: 2026-03-01 / Codex
- Decision: Exclude `.git` entirely from sandbox workspace copies.
  Rationale: Satisfies Epic safety requirement by preventing repository metadata/config credentials from entering untrusted sandbox execution.
  Date/Author: 2026-03-01 / Codex

## Outcomes & Retrospective

Epic 4 acceptance criteria are met in core: cloning and revision checkout are deterministic, sandbox workspace export is generated, and tests prove both branch and commit-SHA behaviors. The adapter is intentionally host-only and can be wired into future run orchestration epics without changing its safety assumptions.

## Context and Orientation

Before this epic, `crates/core` had domain and persistence modules but no source-control adapter. The new git module introduces:

- `crates/core/src/git.rs`: trusted clone + revision checkout + sandbox export behavior.
- `crates/core/src/lib.rs`: module export for `git`.

“Trusted workspace” means directories on the host where sensitive metadata can exist. “Sandbox workspace” means the copy passed to untrusted execution in later epics.

## Plan of Work

Implement `GitAdapter` as a small abstraction that accepts validated domain values (`RepoRef`, `Revision`) and a stable workspace identifier. It should clone to `<root>/<workspace_id>/trusted-clone`, checkout revision, then copy files into `<root>/<workspace_id>/sandbox-workspace` while skipping `.git`. Add tests that build a temporary local git repository, make commits, and assert checked-out file contents.

## Concrete Steps

From repository root:

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features
    npm run build

Expected key results:

- Rust checks pass.
- `npm run build` currently fails with ENOENT because the repository has no `package.json` (recorded as environment limitation).

## Validation and Acceptance

Acceptance is satisfied when:

- Preparing a workspace for revision `main` yields trusted/sandbox copies with `README.md` matching that branch.
- Trusted clone contains `.git`, while sandbox workspace does not.
- Preparing a workspace for a specific commit SHA yields file contents from that exact commit.
- The behavior above is covered by automated tests in `crates/core/src/git.rs`.

## Idempotence and Recovery

`GitAdapter::prepare_workspace` is idempotent for a fixed `workspace_id`: it deletes and recreates the workspace tree each run. If a run is interrupted, rerunning the same call is safe and restores a clean deterministic output.

## Artifacts and Notes

Representative success condition from tests:

    assert!(prepared.trusted_clone_dir.join(".git").exists());
    assert!(!prepared.sandbox_workspace_dir.join(".git").exists());

## Interfaces and Dependencies

Added public API in `crates/core/src/git.rs`:

- `pub struct GitAdapter`
- `pub fn GitAdapter::new(trusted_workspace_root: impl AsRef<Path>) -> Self`
- `pub fn GitAdapter::prepare_workspace(&self, repo_ref: &RepoRef, revision: &Revision, workspace_id: &str) -> Result<PreparedWorkspace, GitError>`
- `pub struct PreparedWorkspace { pub trusted_clone_dir: PathBuf, pub sandbox_workspace_dir: PathBuf }`
- `pub enum GitError`

The adapter depends on host `git` executable availability and standard library filesystem/process APIs.

Revision note (2026-03-01): Initial Epic 4 ExecPlan captured at implementation completion to preserve assumptions, decisions, and validation steps.
