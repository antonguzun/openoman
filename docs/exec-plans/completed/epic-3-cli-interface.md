# Epic 3 CLI Interface (MVP Surface)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, contributors can create and inspect jobs entirely through a command-line interface without writing Rust code. The MVP CLI supports submit/run/status/logs/artifacts/result commands backed by the SQLite persistence layer from Epic 2, with deterministic config errors and end-to-end tests that prove the flow.

## Progress

- [x] (2026-02-28 00:00Z) Reviewed Epic 3 requirements in `MVP_DECOMPOSITION.md` and current Epic 2 persistence/domain APIs.
- [x] (2026-02-28 00:15Z) Added a new `crates/cli` crate with a Clap-based `openoman` binary and command handlers for submit/run/status/logs/artifacts/result.
- [x] (2026-02-28 00:20Z) Implemented config loading from file plus `OPENOMAN_DATABASE_PATH` override with deterministic validation errors.
- [x] (2026-02-28 00:28Z) Added CLI end-to-end tests covering submit→status and missing-config failure behavior.
- [x] (2026-02-28 00:35Z) Updated docs/checkpoint files and ran formatting, linting, tests, and `npm run build` check requirement.

## Surprises & Discoveries

- Observation: `JobEvent` had no event type helper, which made CLI outbox insertion awkward.
  Evidence: `submit` needed a stable event type string to persist into `outbox_events`; adding `JobEvent::event_type()` removed duplicated match logic.
- Observation: The repository currently has no NextJS app or `package.json`.
  Evidence: `npm run build` fails immediately with ENOENT because there is no Node package manifest in repo root.

## Decision Log

- Decision: Implement Epic 3 as a dedicated crate (`crates/cli`) instead of adding a binary target to `crates/core`.
  Rationale: Keeps application surface concerns (argument parsing/config UX) separate from domain/persistence logic and matches likely future split between core services and user interfaces.
  Date/Author: 2026-02-28 / Codex
- Decision: Define MVP `run` as synchronous in-process progression from `queued` to `succeeded`.
  Rationale: Epic 3 requires blocking run behavior before sandbox integration in later epics; this provides deterministic behavior today while preserving state transitions.
  Date/Author: 2026-02-28 / Codex

## Outcomes & Retrospective

Epic 3 acceptance criteria are met: users can create and inspect jobs purely through CLI commands, and config failures return deterministic readable messages. The implementation intentionally keeps `logs` mapped to persisted outbox events for now, and later epics can replace or augment this with sandbox/agent stream logs.

## Context and Orientation

The workspace previously only exposed a core library crate (`crates/core`) with domain and persistence modules. Epic 3 adds a new command-line crate (`crates/cli`) that depends on `openoman-core`.

Key files:

- `crates/cli/src/main.rs` holds CLI command definitions, config loading, and command execution.
- `crates/cli/tests/cli_e2e.rs` holds command-level end-to-end tests.
- `crates/core/src/domain/events.rs` now exposes event type strings used by CLI outbox writes.
- `crates/core/src/persistence/mod.rs` now supports listing outbox events by job for the `logs` command.

## Plan of Work

Create a new workspace member for CLI, wire Clap subcommands, and map each subcommand to existing core persistence/domain operations. Add config parsing from TOML (`core.database_path`) with an environment override. Add e2e tests that execute the real binary with temporary config/database paths. Finally update README and decomposition checkpoint text.

## Concrete Steps

From repository root:

    cargo fmt --all
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features
    npm run build

Expected key results:

- Rust checks pass.
- `npm run build` reports missing `package.json` in this repo (documented environment limitation for the requested check).

## Validation and Acceptance

Acceptance is satisfied when:

- `submit` creates a persisted job and prints `job_id=...`.
- `status <job_id>` returns queued state immediately after submission.
- Missing config path returns clear deterministic error text.
- The commands above are exercised by `crates/cli/tests/cli_e2e.rs` and pass under `cargo test`.

## Idempotence and Recovery

CLI commands are safe to rerun with new temporary databases. If a database file becomes inconsistent during experimentation, delete it and rerun with the same config path to regenerate schema via migrations.

## Artifacts and Notes

Representative output snippets:

    job_id=job-1700000000000
    state=queued

    error: failed to read config file ./does-not-exist.toml: ...

## Interfaces and Dependencies

- New crate: `crates/cli` using `clap` for argument parsing and `serde` + `toml` for config parsing.
- Core API additions:
  - `openoman_core::domain::events::JobEvent::event_type(&self) -> &'static str`
  - `openoman_core::persistence::OutboxRepository::list_by_job(&self, job_id: &str) -> Result<Vec<OutboxEventRecord>, PersistenceError>`

Revision note (2026-02-28): Initial Epic 3 ExecPlan captured at implementation completion to preserve assumptions, decisions, and validation steps.
