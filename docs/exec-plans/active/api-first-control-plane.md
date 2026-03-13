# API-First Control Plane, Adapter Integrations, and Plugin Removal

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` is checked into this repository and this document will be maintained in accordance with it.

## Purpose / Big Picture

After this change, a user can run a single `openoman serve` process that exposes an HTTP control plane for job submission and inspection while continuing to execute jobs inside the trusted core. Telegram, Slack, Jira, Linear, and custom web UIs are no longer modeled as in-process or managed plugins; they are external adapters that call the HTTP API. This gives the project a stable product boundary without pulling third-party integration logic into the trusted runtime.

The behavior is observable from the outside. Starting `openoman serve` should expose `GET /health`, `POST /jobs`, and inspection endpoints for job state, logs, artifacts, and result data. Existing CLI commands continue to work, and the architecture/design docs no longer describe the removed plugin protocol direction.

## Progress

- [x] (2026-03-13 12:05Z) Reviewed current CLI/core/config/persistence architecture, confirmed `RunJobUseCase` is the right orchestration seam, and locked the all-in-one `serve` topology.
- [x] (2026-03-13 12:09Z) Created this ExecPlan in `docs/exec-plans/active/` before code changes.
- [x] (2026-03-13 12:24Z) Implemented shared job service helpers and a `JobRepository::list()` query so CLI and HTTP handlers share the same behavior contract.
- [x] (2026-03-13 12:33Z) Added `[server]` config, `openoman serve`, `axum` handlers, bearer-token middleware, and HTTP tests for health/auth/create/list flows.
- [x] (2026-03-13 12:42Z) Renamed plugin-era request value module to `domain/request.rs`, removed plugin-protocol docs, and rewrote top-level architecture/design docs around control plane + adapters.
- [x] (2026-03-13 12:49Z) Ran formatting plus full workspace validation (`cargo test --all-targets --all-features`) after fixing one stale GitLab expectation and one SQLite test-path issue.

## Surprises & Discoveries

- Observation: The existing application layer already concentrates run orchestration in `RunJobUseCase`, so the HTTP server can reuse trusted execution behavior without duplicating the run path.
  Evidence: `crates/core/src/application/mod.rs` exposes `RunJobUseCase::run`, and `crates/cli/src/app.rs` is currently a thin dispatcher around config/store/use-case setup.

- Observation: The persistence layer lacks a `list jobs` query, which is required for `GET /jobs`.
  Evidence: `crates/core/src/persistence/mod.rs` exposes `JobRepository::load`/`create`/`update` but no listing API.

- Observation: “plugin” appears in both documentation and low-level type names even though the current user-confirmed direction removes plugin runtime work.
  Evidence: `crates/core/src/domain/plugin.rs`, references from `domain/job.rs` and `persistence/mod.rs`, plus `ARCHITECTURE.md`, `docs/design-docs/index.md`, `docs/design-docs/domain-model.md`, `docs/design-docs/roadmap.md`, and `MVP_DECOMPOSITION.md`.

- Observation: The existing `SqliteStore` wrapper uses `Rc<RefCell<Connection>>`, which is not suitable for `axum` application state.
  Evidence: HTTP handler design required a `Send + Sync` state object, so the implementation keeps `AppConfig` in server state and opens a fresh `SqliteStore` per request instead of sharing one connection across handlers.

- Observation: A pre-existing config test expected `platform = "gitlab"` to preserve a self-hosted push URL from `repo_ref`, but the actual runtime intentionally normalizes GitLab cloud aliases to `gitlab.com`.
  Evidence: `cargo test -p openoman-cli` initially failed in `app_config_publish_plan_resolves_gitlab_cloud_and_missing_token_warning` until the assertion was aligned with the existing publish-plan logic.

- Observation: Using `NamedTempFile` directly for a new SQLite round-trip test caused a read-only write failure in the test environment.
  Evidence: `cargo test --all-targets --all-features` failed with `attempt to write a readonly database` until the test switched to a tempdir-backed database path.

## Decision Log

- Decision: Ship the first control-plane runtime as one `openoman serve` process that owns HTTP API and job execution.
  Rationale: Keeps user startup simple and does not force queue/worker distribution work into the first API release.
  Date/Author: 2026-03-13 / Codex

- Decision: Model Slack/Telegram/Jira/Linear/custom UI as external adapters over HTTP rather than managed plugins.
  Rationale: Preserves core trust boundaries and avoids implementing plugin discovery, runtime lifecycle, and versioning before there is a stable control-plane API.
  Date/Author: 2026-03-13 / Codex

- Decision: Remove plugin-specific product documentation and rename low-level value-object modules away from `plugin`.
  Rationale: The old plugin protocol direction is explicitly obsolete for the current architecture and would otherwise leave misleading public documentation and type names.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The API-first refactor shipped end-to-end in this branch:

- `openoman serve` now starts an `axum` HTTP control plane with health, create/list/get, run, retry, logs, artifacts, and result endpoints.
- CLI commands still work and now sit on top of a shared operator service instead of duplicating storage/orchestration details.
- The trusted-core/request vocabulary no longer uses the `plugin` module for request values.
- Plugin-era documentation was removed or rewritten in favor of a control-plane-and-adapters model.
- The runtime stays operationally simple: one process owns HTTP API and execution, while external bots/UIs integrate over HTTP.

## Context and Orientation

The current workspace has two crates: `crates/core` holds the trusted domain, persistence, execution, and publishing logic; `crates/cli` holds the CLI entrypoint, config loading, and command dispatch. Today users operate the system only through CLI commands such as `submit`, `run`, `status`, `logs`, `artifacts`, and `result`. The repo already persists jobs, artifacts, and outbox events in SQLite, and `RunJobUseCase` already encapsulates end-to-end trusted execution and publish behavior.

Key files for this change:

- `crates/cli/src/app.rs`: current runtime bootstrap and command handlers that will gain `serve` and shared service helpers.
- `crates/cli/src/config.rs`: config parsing; will gain server settings.
- `crates/cli/src/cli.rs`: clap surface; will gain `serve`.
- `crates/core/src/persistence/mod.rs`: add a job listing query for HTTP.
- `crates/core/src/domain/plugin.rs`: obsolete module to rename/remove as part of vocabulary cleanup.
- `README.md`, `ARCHITECTURE.md`, and `docs/design-docs/*.md`: update architecture and runtime story from plugins to adapters and HTTP API.

Terms used in this document:

- “control plane” means the trusted HTTP API that accepts job requests and exposes job state.
- “adapter” means an external client or service that translates some other interface (Telegram, Slack, Jira, custom UI) into calls to the control plane.
- “all-in-one serve mode” means one process provides both API and execution rather than splitting API and worker into separate binaries/processes.

## Plan of Work

First, create a small shared service layer in `crates/cli/src/app.rs` (or a sibling module) that can perform submit/list/get/logs/artifacts/result/run operations and return structured data. Reuse existing core domain/persistence/application types; do not duplicate orchestration or invent a second domain model. Add the missing persistence query to list jobs with the same shape returned by `load`.

Second, extend config and CLI surface for server mode. Add a `[server]` config section with host, port, and optional bearer token, parse it in `AppConfig`, and add `openoman serve`. Implement an `axum` router with JSON handlers for `health`, job creation, job listing, job detail, run, retry, logs, artifacts, and result. Use shared service helpers so CLI and HTTP paths stay behaviorally aligned.

Third, add tests. Cover config loading for the new server section, HTTP submit/get/list/run flows, auth failure when a bearer token is configured, and regression coverage that existing CLI commands still work. Prefer focused integration tests in `crates/cli/tests/`.

Fourth, remove plugin-era architecture. Rename `crates/core/src/domain/plugin.rs` to a neutral module name for request policy values, update imports, and rewrite or delete plugin-protocol documentation and references. Update README, architecture docs, and roadmap language to describe adapters over HTTP and the new `serve` runtime.

## Concrete Steps

From repository root `/home/antonguzun/Work/personal/openoman`:

1. Edit core persistence/domain modules to support neutral policy names and job listing.
2. Edit CLI config, clap surface, runtime bootstrap, and add HTTP handlers/server state.
3. Add/update integration tests in `crates/cli/tests/`.
4. Update `config.example.toml`, `README.md`, `ARCHITECTURE.md`, `MVP_DECOMPOSITION.md`, and relevant `docs/design-docs/*` pages.
5. Run targeted validation while iterating:
   - `cargo test -p openoman-core persistence::tests`
   - `cargo test -p openoman-cli config::tests`
   - `cargo test -p openoman-cli --test cli_e2e`
6. Run final validation:
   - `cargo test --all-targets --all-features`

Expected result: targeted tests pass during iteration, then the full workspace test suite passes with the new API-first runtime and updated docs.

## Validation and Acceptance

Acceptance is met when all of these are true:

- `openoman serve` starts successfully with valid config and exposes `GET /health`.
- `POST /jobs` creates a persisted job and returns its identifier and initial queued state.
- `GET /jobs` lists persisted jobs and `GET /jobs/:id` returns the same state/details CLI reports.
- `POST /jobs/:id/run` executes the existing trusted run flow and persists the resulting state, logs, artifacts, and publish metadata/warnings.
- `GET /jobs/:id/logs`, `GET /jobs/:id/artifacts`, and `GET /jobs/:id/result` return persisted data for completed jobs.
- If a bearer token is configured, unauthenticated HTTP requests fail and authenticated requests succeed.
- Existing CLI commands still work for local operator usage.
- Top-level documentation describes adapters and the HTTP control plane; obsolete plugin docs and plugin-specific domain naming are gone.

## Idempotence and Recovery

These changes are safe to iterate on because they are additive runtime/config/doc updates plus a low-risk module rename. Re-running tests is safe. If a SQLite schema/query change is wrong, fix the repository method and rerun tests against a fresh temp database. If HTTP handler wiring is wrong, use CLI e2e plus targeted HTTP tests to isolate routing versus domain logic. Documentation cleanup is recoverable by re-reading the current architecture docs and ensuring no stale plugin-runtime references remain outside historical ExecPlans.

## Artifacts and Notes

Validation artifacts:

- `cargo test -p openoman-cli`: pass (36 unit tests + 16 CLI e2e tests, including HTTP server tests)
- `cargo test --all-targets --all-features`: pass (full workspace)

Representative HTTP behavior from tests:

    POST /jobs
    -> 201 Created
    -> {"job_id":"job-...","state":"queued",...}

    GET /health
    -> 200 OK
    -> {"ok":true}

## Interfaces and Dependencies

New or changed interfaces expected at completion:

- In `crates/cli/src/cli.rs`, add:

    Serve

- In `crates/cli/src/config.rs`, add server runtime config with:

    pub(crate) struct ServerRuntimeConfig {
        pub(crate) bind_addr: std::net::SocketAddr,
        pub(crate) auth_token: Option<String>,
    }

- In `crates/core/src/persistence/mod.rs`, add a job listing API:

    pub fn list(&self) -> Result<Vec<Job>, PersistenceError>

- In the CLI/runtime layer, add shared structured helpers for:
  - submit job
  - list jobs
  - get job
  - run job
  - get logs
  - list artifacts
  - get result

- Add `axum`-based HTTP handlers that expose those operations as JSON endpoints.

Change note (2026-03-13): Initial ExecPlan authored before implementation to satisfy repository requirement for significant feature work and to lock the user-approved architecture shift from plugins to adapters.
Change note (2026-03-13): Updated Progress, Discoveries, Outcomes, and Artifacts after implementation, formatting, and successful full-workspace validation.
