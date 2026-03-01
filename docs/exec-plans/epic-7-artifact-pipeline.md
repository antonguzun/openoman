# Epic 7 Artifact Pipeline (Canonical Patch + Stored Logs/Report)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, `openoman run <job_id>` no longer trusts a guest-authored patch file. The sandbox exports the modified workspace plus report/log text, and the trusted host generates the canonical unified diff from the trusted clone versus exported guest workspace. A user can inspect stored artifacts and see a real repository diff, stored logs, and a sanitized Git worktree inside the sandbox input.

## Progress

- [x] (2026-03-01 13:04Z) Reviewed current `git`, `sandbox`, CLI orchestration, artifact persistence, and CLI tests against the Epic 7 requirements.
- [x] (2026-03-01 13:18Z) Implemented sanitized sandbox worktree preparation with writable `.git` and trusted host-side canonical patch generation.
- [x] (2026-03-01 13:23Z) Replaced guest-authored patch collection with stable per-attempt output export from the runner.
- [x] (2026-03-01 13:31Z) Updated CLI orchestration, artifact persistence, and `logs` command to use stored sandbox artifacts and preserve failure artifacts.
- [x] (2026-03-01 13:37Z) Added unit and CLI end-to-end coverage for sanitized `.git`, canonical diffs, stored logs, and failure handling.
- [x] (2026-03-01 13:38Z) Pinned `tempfile` to `=3.12.0` and updated `Cargo.lock` so the repository builds with local Cargo 1.84.0.
- [x] (2026-03-01 13:40Z) Ran `cargo fmt --all`, `cargo test --all-targets`, and one real `cargo run submit` / `cargo run run` flow against `git@github.com:antonguzun/tsight-agent.git`.

## Surprises & Discoveries

- Observation: the current runtime marks jobs succeeded even when the sandbox never produces a repository diff from guest filesystem state.
  Evidence: `crates/core/src/sandbox.rs` writes a synthetic `EPIC6_AGENT.txt` patch and `crates/cli/src/main.rs` persists it as `sandbox.patch`.
- Observation: Epic 4 explicitly encoded “no `.git` in sandbox workspace”, so this feature must intentionally replace an earlier safety decision rather than extending it invisibly.
  Evidence: `docs/exec-plans/completed/epic-4-git-operations.md` records that decision, and `crates/cli/tests/cli_e2e.rs` asserts `.git` is absent today.
- Observation: the repository’s existing `tempfile = "3.12"` constraint resolves to `tempfile 3.26.0`, which pulls in `getrandom 0.4.1` and fails to parse on Cargo 1.84.0.
  Evidence: `cargo test` initially failed with `feature edition2024 is required` from `getrandom v0.4.1`; pinning `tempfile` to `=3.12.0` removed the blocker.
- Observation: a real `codex exec` run works inside the current runner, but the CLI emits warnings about the user’s Codex state database migrations.
  Evidence: stored `sandbox.logs` for `job-1772370767502` contain repeated `failed to open state db at /home/antonguzun/.codex/state_5.sqlite: migration 11 was previously applied but is missing in the resolved migrations`.

## Decision Log

- Decision: generate `sandbox.patch` only on the trusted host from the trusted clone plus exported guest workspace.
  Rationale: this makes the stored diff deterministic and independent of guest Git metadata or agent honesty.
  Date/Author: 2026-03-01 / Codex
- Decision: provide a sanitized writable `.git` directory inside the sandbox input instead of a read-only or absent `.git`.
  Rationale: normal Git ergonomics inside the guest require local index and metadata updates; sanitization removes remotes and credential-bearing config while preserving local history inspection.
  Date/Author: 2026-03-01 / Codex

## Outcomes & Retrospective

Epic 7 now stores a real artifact pipeline instead of a guest-authored placeholder patch. The sandbox input is a sanitized writable Git worktree, the runner exports stable per-attempt outputs, the trusted host generates the canonical unified diff, and `logs <job_id>` prints stored sandbox logs. Full automated coverage passes locally with `cargo test --all-targets`, and a real `codex exec` run against `tsight-agent` completed successfully through the new pipeline.

## Context and Orientation

The trusted Git adapter lives in `crates/core/src/git.rs`. It currently clones into `<trusted_workspace_root>/<job_id>/trusted-clone` and copies a Git-less sandbox snapshot into `sandbox-workspace`.

The runner lives in `crates/core/src/sandbox.rs`. It currently copies the prepared workspace into a guest directory, writes synthetic patch/report/log files, and copies only those text artifacts back to the host.

The CLI orchestration lives in `crates/cli/src/main.rs`. It currently prepares the workspace, runs the placeholder sandbox, stores the prepared workspace plus guest-authored artifacts, and prints outbox rows for `logs`.

## Plan of Work

Update `crates/core/src/git.rs` so sandbox workspaces are full cloned worktrees with sanitized `.git` metadata. Add a host-side helper that writes a canonical patch by rebuilding a temporary trusted Git worktree from the exported guest workspace and running `git diff --cached HEAD --binary --full-index --no-ext-diff --no-color --no-renames`.

Update `crates/core/src/sandbox.rs` so the runner exports the modified guest workspace, report, and logs into a stable per-attempt directory under the configured sandbox runtime root. Remove guest patch synthesis entirely.

Update `crates/cli/src/main.rs` so `run` starts an attempt before waiting on the sandbox, always tries to collect stable output, enforces artifact size limits, generates `sandbox.patch` on the trusted host, stores `workspace.sandbox_input` and `workspace.sandbox_result`, and marks the job failed when sandbox execution or patch generation fails. Update `logs` to print stored `sandbox.logs` content when present.

Update tests in `crates/core/src/git.rs`, `crates/core/src/sandbox.rs`, and `crates/cli/tests/cli_e2e.rs` to cover sanitized `.git`, canonical diff generation, stable output persistence, stored log printing, and a failing sandbox attempt that still yields collected artifacts.

## Concrete Steps

From the repository root:

    cargo fmt --all
    cargo test --all-targets

Expected observable results after implementation:

- `openoman artifacts <job_id>` lists `workspace.trusted_clone`, `workspace.sandbox_input`, `workspace.sandbox_result`, `sandbox.patch`, `sandbox.report`, and `sandbox.logs`.
- Opening the stored `sandbox.patch` shows a diff against repository files such as `README.md`.
- `openoman logs <job_id>` prints the stored log file contents instead of only outbox rows when sandbox logs exist.

## Validation and Acceptance

Acceptance is satisfied when:

1. The sandbox input workspace contains a sanitized `.git` directory with no remote URL in `.git/config`.
2. The runner exports `workspace-result`, `report.txt`, and `logs.txt` into a stable per-attempt directory that survives `stop()`.
3. The trusted host writes the only persisted patch artifact, and it is a real diff against repository files.
4. `logs <job_id>` prints stored sandbox logs when available.
5. A failing sandbox attempt persists logs/report/workspace result and ends with job state `failed`.

## Idempotence and Recovery

Workspace preparation remains idempotent because `GitAdapter::prepare_workspace` recreates the per-job workspace tree from scratch. Stable per-attempt output directories should be overwritten on rerun of the same job/attempt path. If a run fails midway, deleting the temporary SQLite database and workspace/runtime directories is sufficient to reset the local test environment.

## Artifacts and Notes

Expected artifact refs after implementation:

    workspace.trusted_clone
    workspace.sandbox_input
    workspace.sandbox_result
    sandbox.patch
    sandbox.report
    sandbox.logs

Expected stable runtime path shape:

    <sandbox_runtime_dir>/jobs/<job_id>/attempt-<attempt_id>/workspace-result
    <sandbox_runtime_dir>/jobs/<job_id>/attempt-<attempt_id>/report.txt
    <sandbox_runtime_dir>/jobs/<job_id>/attempt-<attempt_id>/logs.txt
    <sandbox_runtime_dir>/jobs/<job_id>/attempt-<attempt_id>/patch.diff

## Interfaces and Dependencies

Core sandbox interface target:

    pub struct CollectedSandboxOutput {
        pub modified_workspace_dir: PathBuf,
        pub report_path: PathBuf,
        pub logs_path: PathBuf,
    }

    pub trait SandboxRunner {
        fn start(&mut self, spec: AttemptSpec) -> Result<SandboxHandle, SandboxError>;
        fn wait(&mut self, handle: &SandboxHandle) -> Result<SandboxExitStatus, SandboxError>;
        fn collect_output(
            &self,
            handle: &SandboxHandle,
            job_id: &str,
            attempt_id: u32,
        ) -> Result<CollectedSandboxOutput, SandboxError>;
        fn stop(&mut self, handle: &SandboxHandle) -> Result<(), SandboxError>;
    }

Trusted Git helper target:

    pub fn write_canonical_patch(
        trusted_clone_dir: &Path,
        modified_workspace_dir: &Path,
        output_path: &Path,
    ) -> Result<(), GitError>;

Revision note (2026-03-01): Created this plan at implementation start because Epic 7 changes cross trusted Git preparation, sandbox export behavior, CLI orchestration, artifact storage, and tests.
Revision note (2026-03-01): Updated progress and discoveries after implementation, test validation, the `tempfile` compatibility pin, and a real `codex exec` end-to-end verification run.
