# Global LLM Git Naming With Persisted Branch Identity

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` is checked into this repository and this document must be maintained in accordance with it.

## Purpose / Big Picture

After this change, openoman can generate branch names and commit messages through a trusted host-side LLM configuration instead of always deriving them from the job ID. The operator controls one global prompt template that applies across every configured git account and platform. Each job stores its chosen branch name before the first run, so the system can keep using the same branch identity on later retries and future continuation-oriented flows.

The feature remains backward compatible. If the operator does not provide a naming provider API key, openoman keeps using the old deterministic naming mechanism: branch names remain `<branch_prefix>/<job_id>` and commit messages remain `OpenOMAN job <job_id>`.

## Progress

- [x] (2026-03-16 09:51Z) Reviewed current config, job persistence, publish flow, retry semantics, and documentation to ground the implementation shape.
- [x] (2026-03-16 09:51Z) Created this ExecPlan and locked the agreed product behavior: global prompt, separate host-side naming LLM, persisted branch identity, retry inherits naming, missing API key falls back to the legacy deterministic mechanism.
- [x] (2026-03-16 09:57Z) Implemented core naming module, job domain fields, SQLite schema changes, and publisher signature updates so branch and commit names are no longer invented inside GitHub/GitLab publishers.
- [x] (2026-03-16 10:00Z) Implemented CLI config loading for `[git.naming]`, submit-time LLM resolution, retry inheritance, result/API surfacing, and legacy backfill for older jobs during `get`/`run`/`result`.
- [x] (2026-03-16 10:05Z) Updated docs/examples, added config/server/CLI/core coverage, ran targeted suites, and passed `cargo test --all-targets --all-features`.

## Surprises & Discoveries

- Observation: branch naming is currently hardcoded separately in both GitHub and GitLab publishers.
  Evidence: `crates/core/src/github.rs` and `crates/core/src/gitlab.rs` each build `openoman/<job_id>` locally and commit with `OpenOMAN job <job_id>`.

- Observation: the only branch name persisted today lives inside publish metadata, so it does not exist until after a successful publish.
  Evidence: `crates/core/src/domain/job.rs` stores branch name only inside `PublishResult`, and `crates/core/src/persistence/mod.rs` persists only `publish_branch_name`.

- Observation: retry currently creates a fresh job by copying only request fields.
  Evidence: `crates/cli/src/service.rs::retry_job` calls `Job::submit` with repo/revision/instruction/check profile/publish policy, but no persisted branch identity.

- Observation: preserving the “old mechanism” requires using each repo's existing publish `branch_prefix`, not a new global naming prefix.
  Evidence: repo aliases already carry per-repo `branch_prefix` in `crates/cli/src/config.rs`, and changing that behavior would have silently changed branch names even when naming LLM is disabled.

## Decision Log

- Decision: Generate branch name and commit message at submit time, not during run.
  Rationale: the branch identity must exist before the first publish and must be copyable to retry jobs without re-running naming logic.
  Date/Author: 2026-03-16 / Codex

- Decision: Treat missing naming API key as an explicit opt-out that preserves the old mechanism.
  Rationale: this keeps rollout safe and matches the requested backward compatibility requirement.
  Date/Author: 2026-03-16 / Codex

- Decision: Keep the naming configuration global under `[git.naming]` with no repo/account/platform overrides in v1.
  Rationale: the user explicitly asked for one global setting across all git accounts and platforms.
  Date/Author: 2026-03-16 / Codex

- Decision: Persist `branch_name` and `commit_message` directly on the job in addition to keeping `publish_result.branch_name` for compatibility.
  Rationale: branch identity must exist before publish, while existing API consumers already rely on `publish_result.branch_name`.
  Date/Author: 2026-03-16 / Codex

- Decision: Keep the legacy fallback prefix source in existing repo or legacy publishing config instead of introducing a new global naming prefix.
  Rationale: the user explicitly required “old mechanism” behavior when no naming key is provided.
  Date/Author: 2026-03-16 / Codex

- Decision: Never call the naming LLM during `run`; only `submit` may use it, and `run` backfills missing naming with the legacy mechanism.
  Rationale: older jobs need deterministic repair without introducing new network dependencies or changing their branch identity after creation.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The feature shipped end-to-end. Operators can now opt into trusted host-side LLM naming with one global prompt under `[git.naming]`, while omitting the naming key keeps the previous deterministic branch and commit naming path. Jobs persist `branch_name` and `commit_message` from submit time, `result` exposes them even before publish, and HTTP retry inherits them so continuation-oriented flows can stay on the same branch identity.

Backward compatibility also holds for old persisted jobs. When `run`, `get job`, or `result` loads an older row with missing naming fields, the service backfills the legacy deterministic pair and persists it without calling the naming provider. Validation covered targeted core/config/e2e tests and a final `cargo test --all-targets --all-features` pass.

## Context and Orientation

The runtime configuration is loaded in `crates/cli/src/config.rs`. GitHub and GitLab trusted publishing are implemented in `crates/core/src/github.rs` and `crates/core/src/gitlab.rs`. Run orchestration happens in `crates/core/src/application/mod.rs`, while the CLI/service layer that handles submit, retry, run, and result lives in `crates/cli/src/service.rs`.

The domain model for a job lives in `crates/core/src/domain/job.rs`. A “branch identity” in this plan means the stored branch name that openoman should keep using when it publishes or retries work for the same task. Persistence is SQLite in `crates/core/src/persistence/mod.rs`; schema changes are applied lazily when the store opens.

The public operator-facing surfaces that must reflect the new fields are:

- CLI submit/status/result output in `crates/cli/src/app.rs`
- HTTP API docs in `docs/api/control-plane.md`
- OpenAPI draft in `docs/api/openapi.yaml`
- Config docs in `config.example.toml` and `README.md`

## Plan of Work

First, extend the job domain and persistence schema so every job can store `branch_name` and `commit_message` independently from publish result metadata. Update submit and retry flows so these values are set at job creation time and inherited on retry. Add migration-safe loading so older rows without the new columns continue to rehydrate cleanly.

Second, add a small trusted host-side naming module in the core crate that resolves either an LLM-generated pair or the legacy deterministic pair. In v1, the configurable LLM path uses OpenAI-compatible HTTP calls through the host `curl` binary, mirroring the existing publish-side approach. The resolver must validate returned values and silently fall back to the legacy pair whenever the naming client is disabled, missing an API key, times out, or returns invalid data.

Third, update run and publish wiring so publishers stop inventing names locally and instead receive `branch_name` and `commit_message` from the job. Older jobs are repaired in the service layer by synthesizing and persisting the legacy pair if those fields are absent.

Fourth, expand the CLI and API views so branch name and commit message are visible immediately after submit and remain visible in result responses even before publish metadata exists. Update tests, docs, and examples accordingly.

## Concrete Steps

From repository root `/home/antonguzun/Work/personal/openoman`:

1. Edit:
   - `crates/core/src/domain/job.rs`
   - `crates/core/src/persistence/mod.rs`
   - `crates/core/src/github.rs`
   - `crates/core/src/gitlab.rs`
   - `crates/core/src/application/mod.rs`
   - `crates/cli/src/config.rs`
   - `crates/cli/src/service.rs`
   - `crates/cli/src/app.rs`

2. Add any new core module needed for trusted naming and export it from `crates/core/src/lib.rs`.

3. Extend tests in:
   - `crates/core/src/application/mod.rs`
   - `crates/core/src/persistence/mod.rs`
   - `crates/cli/src/config.rs`
   - `crates/cli/tests/cli_e2e.rs`

4. Update:
   - `config.example.toml`
   - `README.md`
   - `docs/api/control-plane.md`
   - `docs/api/openapi.yaml`

5. Run targeted checks while iterating:
   - `cargo test -p openoman-core persistence::tests`
   - `cargo test -p openoman-core application::tests`
   - `cargo test -p openoman-core github::tests`
   - `cargo test -p openoman-core gitlab::tests`
   - `cargo test -p openoman-cli config::tests`
   - `cargo test -p openoman-cli --test cli_e2e`

6. Run broader verification:
   - `cargo test --all-targets --all-features`

## Validation and Acceptance

Acceptance is met when all of these are true:

- Submitting a job without naming API credentials produces the same branch name and commit message as the old mechanism and stores them on the job immediately.
- Submitting a job with valid naming credentials stores LLM-generated values that are later used by GitHub and GitLab publish flows.
- Retrying a job copies the original `branch_name` and `commit_message` into the new queued job.
- `openoman result <job_id>` returns `branch_name` and `commit_message` even before publish metadata exists.
- Existing jobs created before the schema change can still run and publish successfully, with missing naming fields backfilled from the legacy mechanism.

## Idempotence and Recovery

Schema changes are additive through `ALTER TABLE ... ADD COLUMN`, so reopening the store is safe. Re-running tests is safe. If the naming client path is misconfigured, removing or unsetting the naming API key returns the system to the old deterministic behavior without further code changes or destructive recovery steps.

## Artifacts and Notes

Representative legacy behavior that must remain available when no key is configured:

    branch_name = "openoman/<job_id>"
    commit_message = "OpenOMAN job <job_id>"

Representative new result shape after submit/result:

    job_id=<id> result=in_progress
    branch_name=<stored branch>
    commit_message=<stored commit message>

## Interfaces and Dependencies

At completion, the following interfaces must exist or be updated:

- In `crates/core/src/domain/job.rs`, `Job` and `JobSnapshot` include:

    pub branch_name: Option<String>
    pub commit_message: Option<String>

- In a new core naming module, define a trusted naming resolver that can return:

    pub struct ResolvedGitNaming {
        pub branch_name: String,
        pub commit_message: String,
    }

- In `crates/cli/src/config.rs`, add a runtime representation for global naming config under `[git.naming]` and a resolver method that either calls the LLM path or returns the legacy deterministic pair.

- In `crates/core/src/github.rs` and `crates/core/src/gitlab.rs`, update publisher entrypoints to accept precomputed naming:

    pub fn publish_patch(
        &self,
        branch_name: &str,
        commit_message: &str,
        job_id: &str,
        instruction: &str,
        trusted_clone_dir: &Path,
        patch_path: &Path,
    ) -> Result<..., ...>

Revision note (2026-03-16): Initial ExecPlan created before implementation because this feature touches config, trusted host-side HTTP integrations, persistence, public API shape, and publish behavior.
Revision note (2026-03-16): Updated Progress, Discoveries, Decisions, and Outcomes after implementation completed and the full workspace test suite passed.
