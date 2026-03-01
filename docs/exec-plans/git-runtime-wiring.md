# Git Runtime Wiring

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, running `openoman run <job_id>` will do one piece of real work instead of only moving the job state machine forward: it will prepare a trusted clone and sandbox workspace for the submitted repository and revision. A user will be able to submit a job against a local fixture repository, run it, and then inspect stored artifact records that point at the prepared workspace directories.

## Progress

- [x] (2026-03-01 11:30Z) Reviewed `GitAdapter`, current CLI run flow, persistence surface, and ExecPlan requirements.
- [x] (2026-03-01 11:42Z) Wired CLI config parsing and `run` orchestration to call `GitAdapter::prepare_workspace`.
- [x] (2026-03-01 11:48Z) Persisted prepared trusted and sandbox workspace paths as artifact records and job artifact refs.
- [x] (2026-03-01 11:52Z) Added a CLI end-to-end test that submits a job against a local git fixture, runs it, and asserts workspace artifacts.
- [x] (2026-03-01 11:54Z) Ran `cargo fmt --all`.
- [ ] (2026-03-01 11:56Z) Validation remains blocked by the local Cargo toolchain parsing `getrandom v0.4.1` with `edition2024`.

## Surprises & Discoveries

- Observation: The checked-in config already contains `[git].trusted_workspace_dir`, but the CLI config loader ignores it entirely.
  Evidence: `crates/cli/src/main.rs` only deserializes the `core` section today.
- Observation: The current `run` command is still a placeholder that marks the job successful without any repository I/O.
  Evidence: `crates/cli/src/main.rs` transitions directly from `Queued` to `Succeeded`.
- Observation: Formatting succeeds locally, but any Cargo build or test command still stops before compilation because the installed Cargo version is too old for `getrandom v0.4.1`.
  Evidence: `cargo test -p openoman-cli --test cli_e2e` fails with `feature edition2024 is required` from Cargo `1.84.0`.

## Decision Log

- Decision: Make the first runtime wiring visible by storing the prepared trusted and sandbox directories in the existing artifacts table.
  Rationale: This gives an observable outcome for the new behavior without waiting for later sandbox and validation epics.
  Date/Author: 2026-03-01 / Codex
- Decision: Add a transactional helper in `SqliteStore` for persisting the final job snapshot and prepared workspace artifact records together.
  Rationale: The run path should not succeed in-memory while leaving the database with a job update but no artifact rows, or the reverse.
  Date/Author: 2026-03-01 / Codex
- Decision: Default `trusted_workspace_dir` to `./workspaces/trusted` when the config file omits the `git` section.
  Rationale: The repository already documents that default path, and keeping it as a fallback avoids breaking minimal local configs while still honoring explicit configuration.
  Date/Author: 2026-03-01 / Codex

## Outcomes & Retrospective

The existing git adapter is now part of the executable CLI path: `openoman run <job_id>` prepares a trusted clone, exports a sandbox workspace, stores both paths as artifacts, and then continues through the existing placeholder state transitions. The main remaining gap is environmental rather than implementation-related: this machine’s Cargo version cannot run the new CLI test because dependency resolution stops at a crate that requires `edition2024`.

## Context and Orientation

The relevant entry point is `crates/cli/src/main.rs`. It loads `config.toml`, opens the SQLite store, and handles `submit`, `run`, `status`, `logs`, `artifacts`, and `result`. The existing `run` branch loads a job and advances its state through the domain methods, but it does not prepare any workspace.

The git implementation already exists in `crates/core/src/git.rs`. `GitAdapter::prepare_workspace` clones the repository into `<trusted_workspace_root>/<workspace_id>/trusted-clone`, checks out the requested revision, and copies the working tree into `<trusted_workspace_root>/<workspace_id>/sandbox-workspace` while skipping `.git`.

The persistence layer in `crates/core/src/persistence/mod.rs` already supports storing artifact records with `job_id`, `artifact_ref`, `kind`, `path`, `content_hash`, and `size_bytes`. The same module also stores job artifact refs through the `Job::collect_artifacts` transition.

The CLI end-to-end tests live in `crates/cli/tests/cli_e2e.rs`. They currently cover submission and config errors only, so this file must gain a fixture repository scenario that proves `run` now performs a real clone and checkout.

## Plan of Work

Update `crates/cli/src/main.rs` so `FileConfig` also deserializes a `git` section and `AppConfig` carries a trusted workspace root path. In the `Run` command handler, instantiate `openoman_core::git::GitAdapter` with that path and call `prepare_workspace` using the loaded job's `repo_ref`, `revision`, and `id`.

After workspace preparation succeeds, create stable artifact refs for the trusted and sandbox directories, store matching artifact records through the existing `ArtifactsRepository`, and pass those refs into `job.collect_artifacts`. Leave the later placeholder transitions in place so this remains a minimal integration rather than a full execution-engine rewrite.

Extend `crates/cli/tests/cli_e2e.rs` with a local git fixture repository, a config that includes `git.trusted_workspace_dir`, and an assertion that `run` succeeds and `artifacts` shows both prepared workspace paths. Keep the fixture local so the test does not depend on network access.

## Concrete Steps

From the repository root:

    cargo test -p openoman-cli --test cli_e2e

Expected observable result after implementation:

    test submit_then_run_prepares_git_workspaces ... ok

Observed in this environment:

    cargo test -p openoman-cli --test cli_e2e
    Downloading crates ...
    error: failed to download `getrandom v0.4.1`
    ...
    The package requires the Cargo feature called `edition2024`, but that feature is not stabilized in this version of Cargo (1.84.0 ...)

## Validation and Acceptance

Acceptance is satisfied when a human can:

1. Create a local git repository fixture with a known file on branch `main`.
2. Run `openoman submit --repo <fixture-path> --revision main`.
3. Run `openoman run <job_id>` and observe success.
4. Run `openoman artifacts <job_id>` and observe two artifact records pointing at the trusted clone and sandbox workspace directories.
5. Inspect the stored sandbox workspace path and see the checked-out file content without a `.git` directory.

Automated acceptance is the new CLI end-to-end test that performs the same scenario locally.

## Idempotence and Recovery

`GitAdapter::prepare_workspace` already removes the target workspace directory before recreating it, so rerunning the same job id after resetting job state would replace stale files with a clean copy. This change only adds artifact rows; if a test or manual run needs a clean slate, deleting the temporary SQLite database and temporary workspace directory is sufficient.

## Artifacts and Notes

The expected artifact kinds after wiring are:

    workspace.trusted_clone
    workspace.sandbox

The expected path shape is:

    <trusted_workspace_dir>/<job_id>/trusted-clone
    <trusted_workspace_dir>/<job_id>/sandbox-workspace

## Interfaces and Dependencies

`crates/cli/src/main.rs` should import:

    use openoman_core::git::GitAdapter;
    use openoman_core::persistence::NewArtifactRecord;

`AppConfig` should expose:

    struct AppConfig {
        database_path: PathBuf,
        trusted_workspace_dir: PathBuf,
    }

The `Run` handler should use `GitAdapter::prepare_workspace(&job.repo_ref, &job.revision, job.id.as_str()) -> Result<PreparedWorkspace, GitError>` and map failures into deterministic CLI errors.

Revision note (2026-03-01): Created this plan because wiring the existing git adapter into the runtime crosses config loading, orchestration, persistence, and CLI tests.
Revision note (2026-03-01): Updated progress, decisions, and validation notes after implementing the runtime wiring and hitting the existing Cargo `edition2024` blocker during test execution.
