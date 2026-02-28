# Epic 1 Core Domain Model and Job State Machine

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` are kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the core crate has a first real domain model for jobs and state transitions. A developer can create a job aggregate, move it through the intended lifecycle, and observe strongly typed domain events emitted at each key transition. This unlocks Epic 2 persistence because there is now a concrete in-memory model to store and reload.

## Progress

- [x] (2026-02-28 00:25Z) Reviewed Epic 1 requirements in `MVP_DECOMPOSITION.md` and existing core crate baseline.
- [x] (2026-02-28 00:35Z) Added `domain` module structure with `job`, `plugin`, and `events` modules plus value objects.
- [x] (2026-02-28 00:42Z) Implemented `Job` aggregate state machine, attempt invariants, and event emission methods.
- [x] (2026-02-28 00:47Z) Added unit tests for happy path, invalid transition handling, active-attempt invariant, and publish-gate invariant.
- [x] (2026-02-28 00:50Z) Updated Epic 1 checkpoint in `MVP_DECOMPOSITION.md` and ran format/lint/tests.

## Surprises & Discoveries

- Observation: The state sequence includes intermediate states (`CollectingArtifacts`, `Notifying`) that are not terminal outputs themselves but are useful for future persistence and observability.
  Evidence: The happy-path test drives all intermediate states and verifies final `Succeeded`.

## Decision Log

- Decision: Keep all Epic 1 domain types in `crates/core/src/domain/` as plain Rust types without external crates.
  Rationale: This keeps the domain layer deterministic and easy to persist in Epic 2 without additional serialization choices yet.
  Date/Author: 2026-02-28 / Codex

- Decision: Model event emission as return values from state transition methods.
  Rationale: This makes event production explicit and testable while avoiding premature infrastructure abstractions.
  Date/Author: 2026-02-28 / Codex

## Outcomes & Retrospective

Epic 1 goals are met: value objects exist, the job aggregate enforces lifecycle transitions and invariants, and tests verify both valid and invalid flows. The next step is persistence wiring in Epic 2 so these transitions become durable.

## Context and Orientation

The core crate previously contained only smoke tests and no domain model. Epic 1 introduces:

- `crates/core/src/domain/job.rs`: job aggregate, state machine, attempts, value objects (`JobId`, `RepoRef`, `Revision`, `ArtifactRef`), and domain errors.
- `crates/core/src/domain/plugin.rs`: plugin-facing value objects (`CheckProfile`, `PublishPolicy`).
- `crates/core/src/domain/events.rs`: internal domain event enum for job lifecycle events.
- `crates/core/src/lib.rs`: exports the `domain` module.

A “job aggregate” here means one root type (`Job`) that owns state and enforces business rules through methods. A “domain event” means a typed record returned whenever a meaningful transition occurs.

## Plan of Work

Implement a dedicated `domain` module tree and define minimal strongly typed value objects to avoid passing raw strings through the state machine. Introduce `JobState` variants that match the Epic 1 sequence and implement transition methods in `Job` that enforce valid ordering and one-active-attempt invariant. For each transition that matters externally, return a `JobEvent` value with key identifiers. Add focused tests covering the end-to-end success path and invariant failures. Finally, update the decomposition document with an Epic 1 checkpoint.

## Concrete Steps

From repository root `/workspace/openoman`, run:

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features

Expected behavior: all commands succeed and domain tests in `crates/core/src/domain/job.rs` pass.

## Validation and Acceptance

Acceptance is met when:

- The core crate compiles with domain modules for jobs, plugins, and events.
- Unit tests demonstrate valid transitions through queued-to-succeeded and reject invalid transitions/invariant violations.
- State transition methods return domain events representing lifecycle milestones.

## Idempotence and Recovery

These changes are source-only. Re-running formatting/lint/tests is safe and idempotent. If a transition test fails, inspect the specific method in `job.rs`, adjust invariant checks, and rerun the same commands.

## Artifacts and Notes

Expected test signal includes lines like:

    running 6 tests
    test domain::job::tests::follows_happy_path_and_emits_events ... ok

## Interfaces and Dependencies

No external dependencies were added. The public domain interfaces introduced are:

- `crate::domain::job::Job` with transition methods that return `Result<JobEvent, JobError>`.
- `crate::domain::events::JobEvent` with variants for submission, attempts, artifact collection, validation, PR creation, and terminal outcomes.
- Value object constructors (`new(...) -> Result<_, _>`) that reject empty strings.

Revision note (2026-02-28): Initial Epic 1 ExecPlan created at implementation completion to preserve context, decisions, and validation commands for future contributors.
