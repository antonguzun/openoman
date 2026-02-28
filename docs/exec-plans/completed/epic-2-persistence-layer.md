# Epic 2 Persistence Layer (SQLite) and Transactional Writes

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` are kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the core crate can persist jobs, outbox events, and artifact metadata in SQLite. A developer can submit and mutate a job in memory, save it, restart by reopening the database, and reload the same state. The persistence layer also supports an atomic write path for “job update + outbox insert” so state and integration-event history remain consistent.

## Progress

- [x] (2026-02-28 23:18Z) Reviewed Epic 2 requirements in `MVP_DECOMPOSITION.md` and existing Epic 1 domain model.
- [x] (2026-02-28 23:26Z) Added SQLite-backed persistence module with embedded migrations for jobs, attempts, artifact refs, outbox events, and artifact metadata.
- [x] (2026-02-28 23:30Z) Implemented repositories for jobs, outbox events, and artifacts plus transactional update+outbox operation.
- [x] (2026-02-28 23:36Z) Added integration-style tests validating migration, job round-trip persistence, and transactional writes.
- [x] (2026-02-28 23:37Z) Ran fmt, clippy, and tests for the Rust workspace.

## Surprises & Discoveries

- Observation: The domain aggregate needed hydration support to reconstruct private runtime fields (`active_attempt_id`, `validation_succeeded`) from storage.
  Evidence: Added `JobSnapshot` + `Job::rehydrate(...)` and used it in repository load path.

- Observation: Strict clippy settings reject ad-hoc `from_str` helper method names and long constructor argument lists.
  Evidence: `cargo clippy -- -D warnings` failed until parse methods were renamed and hydration was moved to a snapshot struct.

## Decision Log

- Decision: Keep persistence inside `crates/core` as `persistence` module rather than creating a new crate.
  Rationale: Epic 2 asks for an adapter/module and the current workspace only contains one crate, so module-level separation gives fast progress without cross-crate overhead.
  Date/Author: 2026-02-28 / Codex

- Decision: Use `rusqlite` with the `bundled` feature.
  Rationale: This avoids host SQLite linking variance in CI and local environments.
  Date/Author: 2026-02-28 / Codex

- Decision: Model atomic writes via `SqliteStore::update_job_and_insert_outbox` transaction.
  Rationale: It directly enforces the Epic 2 invariant that job state updates and outbox insertion commit together.
  Date/Author: 2026-02-28 / Codex

## Outcomes & Retrospective

Epic 2 goals are met for MVP persistence baseline: schema migrations run on a clean database, jobs can be created/updated/reloaded, outbox and artifacts repositories exist, and an atomic write path is tested. Next epics can build CLI and execution flow on top of this durable state.

## Context and Orientation

The domain model from Epic 1 lives under `crates/core/src/domain`. Epic 2 introduces a sibling module `crates/core/src/persistence/mod.rs` that owns SQLite schema and repositories. Key pieces:

- `SqliteStore`: opens connection, runs migrations, and provides repository handles.
- `JobRepository`: create/load/update for domain `Job` aggregate with attempts and artifact refs.
- `OutboxRepository`: insert/list/update-status for integration event records.
- `ArtifactsRepository`: insert/list metadata records for collected artifacts.
- `SqliteStore::update_job_and_insert_outbox`: explicit atomic transaction for consistency.

A “migration” here means SQL DDL statements executed on startup to ensure required tables exist before repositories run queries.

## Plan of Work

Implement embedded schema creation in `SqliteStore::run_migrations`, then build repository operations that map domain value objects to rows and back. Add minimal parse/string conversion helpers in domain enums and value objects needed for persistence. Introduce a hydration snapshot for reconstructing `Job` while preserving aggregate invariants. Finally, add integration-style tests using temp SQLite files to verify end-to-end behavior.

## Concrete Steps

From repository root `/workspace/openoman`, run:

    cargo fmt --all
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features

Expected behavior: all commands succeed and persistence tests pass, including migration and round-trip tests.

## Validation and Acceptance

Acceptance is met when:

- Opening a new database runs migrations and creates required tables.
- Saving a submitted job, mutating state, saving again, and reloading returns expected state/attempt/artifact-ref data.
- Transactional method persists job update and outbox event in one commit.

## Idempotence and Recovery

Migrations use `CREATE TABLE IF NOT EXISTS`, so reopening an existing DB is safe. Repository operations are deterministic; rerunning tests is safe and leaves only temporary files that are cleaned by the test harness.

## Artifacts and Notes

Expected test transcript includes:

    test persistence::tests::migration_creates_schema_on_clean_database ... ok
    test persistence::tests::create_update_and_reload_job_round_trips ... ok
    test persistence::tests::job_update_and_outbox_insert_are_atomic ... ok

## Interfaces and Dependencies

New dependency: `rusqlite` (with `bundled`) and dev dependency `tempfile` in `crates/core/Cargo.toml`.

Public persistence interfaces now exported from `openoman_core`:

- `openoman_core::persistence::SqliteStore`
- `openoman_core::persistence::JobRepository`
- `openoman_core::persistence::OutboxRepository`
- `openoman_core::persistence::ArtifactsRepository`
- `openoman_core::persistence::{NewOutboxEvent, OutboxStatus, NewArtifactRecord}`

Revision note (2026-02-28): Initial Epic 2 ExecPlan captured after implementation to preserve context, decisions, and verification details for future contributors.
