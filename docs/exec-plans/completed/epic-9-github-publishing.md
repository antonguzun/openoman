# Epic 9 GitHub Publishing

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, a successful `openoman run <job_id>` can finish the MVP publish path instead of fabricating a placeholder pull request URL. When a job uses publish policy `on_validation_success`, the trusted core will apply the canonical patch to the trusted clone, create a job-specific branch, push that branch, create a GitHub pull request, store the branch and PR metadata on the job, and emit an outbox integration event that other systems can inspect.

The result is observable from the CLI. A successful run should end with a real branch name and pull request URL in `openoman result <job_id>`, and the SQLite outbox should contain a `job.pr_created` event for that job.

## Progress

- [x] (2026-03-02 15:57Z) Reviewed Epic 9 scope, current CLI run flow, domain model, persistence schema, and existing git/runtime plans.
- [x] (2026-03-02 16:10Z) Added publish-result fields to the domain model and SQLite persistence so jobs now store branch name, PR URL, and PR number.
- [x] (2026-03-02 16:19Z) Implemented a trusted GitHub publishing adapter that applies the canonical patch, commits a job branch, pushes it, and creates a pull request.
- [x] (2026-03-02 16:28Z) Wired publishing config and run orchestration in the CLI, including outbox insertion and `publish_policy = "never"` handling.
- [x] (2026-03-02 16:35Z) Added unit and CLI end-to-end coverage for publish success, publish skip, and persisted result output.
- [x] (2026-03-02 16:41Z) Ran `cargo fmt --all`, `cargo test -p openoman-core`, and `cargo test -p openoman-cli --test cli_e2e` with `CARGO_HOME=/tmp/openoman-cargo-home`.

## Surprises & Discoveries

- Observation: The checked-in `config.toml` and `config.example.toml` already contain a placeholder `[publishing]` section, but the CLI ignores it entirely today.
  Evidence: `crates/cli/src/main.rs` deserializes `core`, `git`, `sandbox`, and `agent`, but no publishing config.
- Observation: The current runtime already computes a canonical patch from the clean trusted clone to the sandbox result, which gives the publish step a clean trusted input without copying sandbox state directly.
  Evidence: `crates/cli/src/main.rs` writes `patch.diff` through `openoman_core::git::write_canonical_patch(...)`.
- Observation: Eager publishing validation would have broken existing configs because the checked-in sample and local config both had incomplete placeholder `[publishing]` blocks.
  Evidence: the feature now validates publishing lazily only when a job actually reaches `publish_policy = "on_validation_success"`.
- Observation: Keeping the GitHub API call shell-out based avoided another HTTP client dependency while still giving deterministic tests through a fake `curl` binary.
  Evidence: `crates/core/src/github.rs` accepts `curl_bin`, and both the core and CLI test suites use a local fake script for PR creation.

## Decision Log

- Decision: Persist publish metadata directly on `Job` instead of treating branch and PR values as generic artifacts.
  Rationale: The job aggregate is already responsible for storing publish results, and `result <job_id>` needs a durable, structured source of truth.
  Date/Author: 2026-03-02 / Codex
- Decision: Keep the trusted publish path patch-based: apply the canonical patch to the trusted clone, commit there, and push from that clone.
  Rationale: This preserves the trust boundary described in the design docs and avoids publishing the sandbox filesystem directly.
  Date/Author: 2026-03-02 / Codex
- Decision: Add a distinct "publish skipped" domain transition for `publish_policy = "never"`.
  Rationale: The current state machine forces every validated job through `PullRequestCreated`, which is incompatible with the existing publish policy value object.
  Date/Author: 2026-03-02 / Codex

## Outcomes & Retrospective

Epic 9 is complete. `openoman run <job_id>` now uses a trusted publish path after validation: it replays the canonical patch onto the trusted clone, creates and pushes an `openoman/<job_id>` branch by default, opens a GitHub pull request, stores the branch and PR metadata on the job, and inserts a `job.pr_created` outbox event in the same final transaction as the job/artifact update. Jobs configured with `publish_policy = "never"` now skip publishing cleanly and still succeed.

The main follow-on gap is broader outbox coverage across other lifecycle transitions, which belongs to Epic 10 rather than this change.

## Context and Orientation

The job state machine lives in `crates/core/src/domain/job.rs`. Today it tracks job state, attempts, artifact refs, and whether validation succeeded, but it does not store any publish result. The matching domain events live in `crates/core/src/domain/events.rs`.

SQLite persistence lives in `crates/core/src/persistence/mod.rs`. The `jobs` table currently stores the job request, state, active attempt, and validation flag. The CLI uses `JobRepository::load` and `SqliteStore::update_job_and_insert_artifacts` during `run`.

Trusted git operations live in `crates/core/src/git.rs`. `GitAdapter::prepare_workspace` creates a clean trusted clone and a sanitized sandbox workspace. `write_canonical_patch` already creates the patch file that should be replayed on the trusted clone before publishing.

The runtime orchestration lives in `crates/cli/src/main.rs`. The `Run` command currently prepares the workspaces, runs the sandbox, collects artifacts, marks validation successful, fabricates `https://example.invalid/pr/1`, and then marks the job successful. The CLI config loader does not read any publishing settings yet. The end-to-end tests for the binary live in `crates/cli/tests/cli_e2e.rs`.

## Plan of Work

First, extend the domain model in `crates/core/src/domain/job.rs` with a publish result value type and an optional field on `Job` and `JobSnapshot`. Add a transition that records branch name, PR URL, and PR number when a pull request is created, and add a separate transition that moves a validated job from `Publishing` to `Notifying` without a PR when publish policy is `never`. Update `crates/core/src/domain/events.rs` so the integration event type string for the pull request event is `job.pr_created`.

Next, update `crates/core/src/persistence/mod.rs` so the `jobs` table stores the publish result fields. The migration must be additive for existing databases. Extend `JobRepository::load`, `upsert_job`, and the persistence tests so the new fields round-trip correctly. Add a transactional helper that can persist one job update together with both artifact rows and outbox rows, because a successful publish should not store branch and PR metadata without its matching `job.pr_created` integration event.

Then, add a new trusted publisher adapter in `crates/core/src/github.rs`. It should expose a small configuration struct plus a `GitHubPublisher` type that:

1. Computes a branch name from the job id.
2. Applies the canonical patch file to the trusted clone.
3. Creates and commits the branch in the trusted clone.
4. Pushes that branch to the configured remote without persisting the GitHub token into the repository config.
5. Creates the pull request through the GitHub REST API and returns structured metadata.

Keep the implementation blocking and shell-out based, matching the repository’s existing use of `git` and Firecracker commands. The adapter should support a configurable API base URL and push URL so tests can target a local bare repo and a fake pull request endpoint.

After that, wire the CLI in `crates/cli/src/main.rs`. Add `[publishing]` config parsing, including token loading from environment, default GitHub API URL, and deterministic validation errors. In the `Run` flow, after validation succeeds:

- if `publish_policy` is `never`, mark publish skipped and succeed the job;
- otherwise, publish the canonical patch from the trusted clone, record the publish result on the job, create the `job.pr_created` outbox row, and persist the job, artifacts, and outbox rows atomically.

Finally, extend `crates/cli/tests/cli_e2e.rs` and any needed core tests. The CLI tests should create a local fixture repo plus a local bare remote, run the job end-to-end with a fake GitHub API responder, assert the remote branch exists, assert `result` prints branch and PR metadata, and verify the non-publishing policy path succeeds without creating PR metadata.

## Concrete Steps

From the repository root:

    cargo fmt --all
    cargo test -p openoman-core
    cargo test -p openoman-cli --test cli_e2e

Expected observable result after implementation:

    test submit_run_and_result_show_github_publish_metadata ... ok
    test publish_policy_never_skips_pull_request_creation ... ok

## Validation and Acceptance

Acceptance is met when a human can:

1. Configure `[publishing]` with a GitHub repository, API URL, and token environment variable.
2. Submit a job with `publish_policy = "on_validation_success"`.
3. Run the job and observe `state=succeeded`.
4. Run `openoman result <job_id>` and observe a branch name plus PR number and URL.
5. Inspect the target remote and confirm the new branch exists with the sandbox change committed.
6. Inspect the outbox rows for the job and confirm a `job.pr_created` event exists.
7. Submit another job with `publish_policy = "never"` and observe that it succeeds without PR metadata.

Automated acceptance is the combination of core unit tests for publish-result persistence and CLI end-to-end coverage for both publishing and publish-skip flows.

## Idempotence and Recovery

The implementation should avoid mutating the original remote configuration in the trusted clone so reruns do not accumulate credentials or stale remotes. The SQLite migration must use additive `ALTER TABLE` behavior so it can be applied to an existing database safely. If a publish attempt fails after artifacts were collected, rerunning from a fresh temporary workspace and database should be sufficient recovery for tests; no destructive rollback of user data is required.

## Artifacts and Notes

Expected result output shape after a successful publish:

    job_id=job-123 result=success
    branch=openoman/job-123
    pull_request_number=17
    pull_request_url=https://github.example/repos/acme/demo/pulls/17

Expected outbox event type:

    job.pr_created

## Interfaces and Dependencies

In `crates/core/src/domain/job.rs`, define a stable publish result type:

    pub struct PublishResult {
        pub branch_name: String,
        pub pull_request_url: String,
        pub pull_request_number: u64,
    }

In `crates/core/src/github.rs`, define:

    pub struct GitHubPublisherConfig {
        pub api_base_url: String,
        pub repo_owner: String,
        pub repo_name: String,
        pub base_branch: String,
        pub branch_prefix: String,
        pub push_url: String,
        pub token: String,
        pub curl_bin: String,
    }

    pub struct PublishedPullRequest {
        pub branch_name: String,
        pub pull_request_url: String,
        pub pull_request_number: u64,
    }

    pub struct GitHubPublisher { ... }

    impl GitHubPublisher {
        pub fn new(config: GitHubPublisherConfig) -> Self;
        pub fn publish_patch(
            &self,
            job_id: &str,
            instruction: &str,
            trusted_clone_dir: &Path,
            patch_path: &Path,
        ) -> Result<PublishedPullRequest, GitHubPublishError>;
    }

The CLI config should gain a `publishing` section that resolves to an optional runtime config. When enabled, `crates/cli/src/main.rs` should create a `GitHubPublisher` and use it only in the trusted host flow after validation succeeds.

Revision note (2026-03-02): Created this plan before implementation because Epic 9 is a cross-cutting feature that changes the domain model, persistence schema, runtime orchestration, and test surface.
Revision note (2026-03-02): Updated this plan after implementation to record the shipped design, the lazy publishing-validation decision, and the exact validation commands that passed.
