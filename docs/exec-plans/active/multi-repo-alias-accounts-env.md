# Multi-Repo Alias, Account Binding, and Per-Repo Env Injection

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` is checked into this repository and this document will be maintained in accordance with it.

## Purpose / Big Picture

After this change, a user can configure many repositories in one OpenOMAN config, assign each repository to a specific git account identity, submit jobs by repository alias, and automatically stage local test environment files from `env_for_repo/{repo_name}` into the sandbox workspace. The same run still uses trusted publishing boundaries, but publishing is now chosen per repository: GitHub repositories can publish using the bound account token and identity, while non-GitHub repositories (or GitHub repos without tokens) finish successfully with an explicit publish warning instead of hard failure.

The behavior is observable from the CLI. `submit --repo <alias>` resolves the alias to its configured repository. `run` executes with repo-specific env overlays. `result` shows publish metadata when publishing happened, or `publish_warning=...` when publishing was intentionally skipped.

## Progress

- [x] (2026-03-05 11:06Z) Reviewed current CLI/core/config/persistence architecture and produced a decision-complete implementation plan with user-confirmed defaults.
- [x] (2026-03-05 11:12Z) Created this ExecPlan in `docs/exec-plans/active/` and aligned it with `docs/PLANS.md` requirements.
- [x] (2026-03-05 11:25Z) Implemented core domain/application/persistence updates for `repo_alias`, `publish_warning`, publish-skip warnings, and workspace env overlay wiring.
- [x] (2026-03-05 11:32Z) Implemented CLI/config repo catalog + account binding model, alias submission resolution, and per-job publish resolution.
- [x] (2026-03-05 11:36Z) Updated docs/config examples and added focused tests for alias submit, env overlay, and warning-based publish skips.
- [x] (2026-03-05 11:42Z) Ran targeted and broader test suites (`openoman-core`, `openoman-cli`, and full workspace all-target/all-feature tests).

## Surprises & Discoveries

- Observation: Current patch generation (`write_canonical_patch`) stages all untracked files from sandbox workspace into the canonical patch.
  Evidence: `crates/core/src/git.rs` runs `git add -A .` in a temp workspace copied from sandbox output.

- Observation: Env overlay files would leak into patch output without explicit ignore controls.
  Evidence: Prior to adding `.git/info/exclude` entries for injected file names, canonical patch generation included newly staged `.env*` files.

- Observation: This workspace toolchain rejects Rust 2024-only `let` chains.
  Evidence: `cargo test -p openoman-cli --no-run` failed on `if let ... && let ...` until `resolve_generic_token` was rewritten with nested `if`.

## Decision Log

- Decision: Keep `submit --repo` and resolve alias-first, raw-ref fallback.
  Rationale: Preserves backward compatibility while adding alias support with minimal CLI churn.
  Date/Author: 2026-03-05 / Codex

- Decision: Implement account binding as reusable account registry plus per-repo account link.
  Rationale: Matches user request for many repositories with different git accounts and avoids repeated credentials/config blocks.
  Date/Author: 2026-03-05 / Codex

- Decision: For `publish_policy=on_validation_success`, skip publishing with persisted warning when repo platform is not GitHub or GitHub token is absent.
  Rationale: Matches user-confirmed behavior to avoid hard failures when token is missing.
  Date/Author: 2026-03-05 / Codex

- Decision: Keep legacy `[publishing]` as fallback for raw (non-alias) `submit --repo`.
  Rationale: Preserves backward compatibility while introducing alias-first repo/account configuration under `[git]`.
  Date/Author: 2026-03-05 / Codex

## Outcomes & Retrospective

The feature shipped end-to-end in this branch:

- Jobs now persist optional `repo_alias` and `publish_warning`.
- `submit --repo` resolves alias first and falls back to raw repo refs.
- Config now supports many repositories and reusable git accounts under `[git]`.
- `run` now supports repo-scoped env overlays from `env_for_repo/{repo_name}`.
- Canonical patch generation excludes injected env overlay files by default.
- Publishing plan is now per repo alias: GitHub publishes with account token/identity; non-GitHub or missing-token cases succeed with warnings.

Behavioral acceptance was verified by unit and e2e tests, including a new full-flow e2e test for alias submit + env overlay + publish warning.

## Context and Orientation

OpenOMAN currently stores a single `repo_ref` on each job (`crates/core/src/domain/job.rs`) and submits jobs only via raw `--repo` (`crates/cli/src/cli.rs`, `crates/cli/src/app.rs`). Runtime config is loaded in `crates/cli/src/config.rs`. Publishing is currently global and GitHub-only via `[publishing]` and `GitHubPublisherConfig` (`crates/core/src/github.rs`).

Workspace preparation happens in `GitAdapter::prepare_workspace` (`crates/core/src/git.rs`), and run orchestration is in `RunJobUseCase::run` (`crates/core/src/application/mod.rs`). Persistence is SQLite in `crates/core/src/persistence/mod.rs` with migration-on-open behavior.

"Repository alias" here means a stable, user-chosen key in config that points to a concrete git clone URL/path and account/platform metadata. "Account binding" means selecting which token and git commit identity to use when trusted publish is attempted. "Env overlay" means copying local files from `env_for_repo/{repo_name}` into the sandbox input workspace before agent execution.

## Plan of Work

Implementation proceeds in four groups.

First, extend the core job model and run orchestration. Add optional `repo_alias` and `publish_warning` fields to `Job`/`JobSnapshot` and persist them end-to-end. Update publishing flow in `RunJobUseCase` to accept an explicit publish plan result: publish to GitHub or skip with warning. Add a new job transition helper that marks publishing skipped with warning while preserving successful terminal completion.

Second, extend git workspace preparation for per-repo env overlays. Add a `prepare_workspace_with_env_overlay` path to `GitAdapter` that can copy all regular files from a configured env folder into sandbox workspace root. Ensure these injected files do not leak into canonical patch generation by writing them to git exclude metadata used by canonical patch staging.

Third, extend CLI config and command behavior. Add repo account and repo catalog parsing under `[git]`, keep legacy `[publishing]` support, and add resolver methods for alias submission, env overlay directory lookup, and per-job publish planning. Update `submit` to resolve aliases and persist `repo_alias`, and update `run`/`result` output to surface publish warnings.

Fourth, update documentation and tests. Add/adjust unit tests in core and CLI config, plus CLI e2e coverage for alias submit, env overlay behavior, and warning-based publish skips for non-GitHub/missing-token cases.

## Concrete Steps

From repository root `/home/antonguzun/Work/personal/openoman`:

1. Edit core domain/persistence/application/git modules and their tests.
2. Edit CLI config/app wiring and CLI e2e tests.
3. Update `config.example.toml` and `README.md` for new repo/account config model and alias usage.
4. Run targeted tests while iterating:
   - `cargo test -p openoman-core domain::job`
   - `cargo test -p openoman-core persistence::tests`
   - `cargo test -p openoman-core application::tests`
   - `cargo test -p openoman-core git::tests`
   - `cargo test -p openoman-core github::tests`
   - `cargo test -p openoman-cli config::tests`
   - `cargo test -p openoman-cli --test cli_e2e`
5. Run final verification:
   - `cargo test --all-targets --all-features`

Expected output: all targeted suites pass and final workspace tests pass without mutating tracked files unexpectedly.

## Validation and Acceptance

Acceptance is met when all of these are true:

- Submitting with `--repo <alias>` creates a job with resolved `repo_ref` and stored `repo_alias`.
- Running a job for an aliased repo copies files from `env_for_repo/{repo_name}` into sandbox input without failing when folder is absent.
- Canonical patch generation does not include the injected env overlay files by default.
- For `publish_policy=on_validation_success`:
  - GitHub repo with token publishes and stores PR metadata.
  - GitHub repo without token succeeds with `publish_warning` and no PR metadata.
  - Non-GitHub repo succeeds with `publish_warning` and no PR metadata.
- `openoman result <job_id>` prints `publish_warning=...` when warning exists.

## Idempotence and Recovery

All changes are additive code/config updates and SQLite migration additions. Re-running tests is safe. If migration SQL is incorrect, fix migration helper logic and reopen a fresh temp DB in tests; no destructive schema rewrite is required. If env overlay path behavior is incorrect, set `env_for_repo_dir` to an empty/nonexistent path and rerun to verify graceful no-op behavior.

## Artifacts and Notes

Validation artifacts:

- `cargo test -p openoman-core`: pass (56 tests).
- `cargo test -p openoman-cli config::tests`: pass (25 tests).
- `cargo test -p openoman-cli --test cli_e2e`: pass (15 tests).
- `cargo test --all-targets --all-features`: pass.

Representative new behavior from tests:

- `submit_with_repo_alias_applies_env_overlay_and_reports_publish_warning` verifies:
  - alias resolution into stored `repo_alias`,
  - env overlay files copied into sandbox workspace artifact,
  - `sandbox.patch` excludes overlay `.env` files,
  - `run`/`result` include `publish_warning` for non-GitHub alias publishing.

## Interfaces and Dependencies

New/updated interfaces expected at completion:

- In `crates/core/src/domain/job.rs`, `Job` and `JobSnapshot` include:

    pub repo_alias: Option<String>
    pub publish_warning: Option<String>

- In `crates/core/src/application/mod.rs`, publish resolution uses a plan enum instead of raw optional config:

    pub enum PublishExecutionPlan {
        GitHub(GitHubPublisherConfig),
        SkipWithWarning(String),
    }

- In `crates/core/src/git.rs`, add:

    pub fn prepare_workspace_with_env_overlay(
        &self,
        repo_ref: &RepoRef,
        revision: &Revision,
        workspace_id: &str,
        env_overlay_dir: Option<&Path>,
    ) -> Result<PreparedWorkspace, GitError>

- In `crates/cli/src/config.rs`, add runtime repo/account resolution methods for:
  - alias submit resolution,
  - env overlay directory lookup,
  - per-job publish plan resolution.

Change note (2026-03-05): Initial ExecPlan authored before code changes to satisfy repository requirement for significant feature work and to lock user-confirmed decisions in a single living document.
Change note (2026-03-05): Updated Progress, Discoveries, Decisions, Artifacts, and Outcomes after implementation and successful workspace-wide validation.
