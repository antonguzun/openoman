# Epic 5 — microVM runner lifecycle (MVP local Firecracker adapter)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, `openoman run <job_id>` will execute a real sandbox lifecycle adapter instead of only pretending to run untrusted work. A user will be able to submit a job, run it, and then inspect sandbox-produced artifacts (`patch`, `report`, and `logs`) that come from a disposable runner working directory. This provides the first concrete implementation of Epic 5’s lifecycle contract (`start`, `wait`, `collect_artifacts`, `stop`) while remaining runnable on a normal developer machine.

## Progress

- [x] (2026-03-01 12:08Z) Reviewed Epic 5 requirements, current runtime wiring, and `docs/PLANS.md` expectations.
- [x] (2026-03-01 12:12Z) Implemented a Firecracker-named sandbox runner adapter in `openoman-core` with lifecycle API, timeout handling, copy-in/copy-out via tar, and smoke artifact generation.
- [x] (2026-03-01 12:22Z) Wired CLI `run` command to call the sandbox runner and persist patch/report/log artifacts with workspace records.
- [x] (2026-03-01 12:24Z) Added/updated tests for core runner lifecycle and CLI end-to-end artifact visibility.
- [x] (2026-03-01 12:30Z) Ran formatting and Rust test validation commands; attempted `npm run build` per NextJS instruction and confirmed repository has no `package.json`.

## Surprises & Discoveries

- Observation: Using a tar archive path relative to the source working directory caused intermittent copy failures when tests run in parallel.
  Evidence: `sandbox::tests::wait_marks_timeout_when_process_exceeds_limit` failed with `tar: ... Cannot open: No such file or directory` until archive naming switched to unique temp-file paths.
- Observation: The repository is Rust-only and has no NextJS project root, so `npm run build` cannot execute here.
  Evidence: `npm` returned `ENOENT` for `/workspace/openoman/package.json`.

## Decision Log

- Decision: Implement an MVP `FirecrackerRunner` adapter that models lifecycle behavior locally using subprocesses and disposable directories instead of booting a real microVM.
  Rationale: The repository currently has no guest image build assets or host-level Firecracker wiring; a local adapter still validates interfaces, orchestration, artifact flow, and cleanup semantics required by Epic 5.
  Date/Author: 2026-03-01 / Codex

## Outcomes & Retrospective

Epic 5 MVP behavior is now observable: the CLI run path invokes a concrete sandbox runner lifecycle, generates patch/report/log artifacts through the adapter, persists those artifact records, and passes full Rust test coverage. This does not yet boot a real Firecracker VM image, but it establishes the required core interfaces and lifecycle semantics so a later iteration can swap internals without changing CLI orchestration.

## Context and Orientation

The runtime entry point is `crates/cli/src/main.rs`, specifically the `Run` command branch. It currently prepares git workspaces and then advances domain state directly to success. The core library has `git` and `persistence` adapters but no sandbox adapter module yet.

Epic 5 requires a sandbox lifecycle API and observable artifact collection. In this repository, that means creating a new core adapter module that can be called by CLI orchestration, then persisting produced artifacts through existing SQLite artifact tables.

## Plan of Work

Create `crates/core/src/sandbox.rs` with domain-neutral lifecycle types (`AttemptSpec`, `ResourceLimits`, `SandboxHandle`, `SandboxExitStatus`, `CollectedArtifacts`) and a `SandboxRunner` trait exposing `start`, `wait`, `collect_artifacts`, and `stop`. Implement `FirecrackerRunner` as the concrete adapter for MVP, with deterministic local behavior:

- copy workspace into a disposable guest directory using tar import/export commands,
- run a smoke subprocess that writes patch/report/log artifact files,
- enforce wall-clock timeout in `wait`,
- copy artifacts back out to host-visible output,
- cleanup runner resources in `stop`.

Update `crates/cli/src/main.rs` to instantiate the runner in `Run`, execute the lifecycle, map outputs into `NewArtifactRecord`s, and persist those artifacts with the final job snapshot.

Update tests: add unit tests in `sandbox.rs` for lifecycle smoke flow and timeout behavior, then extend CLI e2e assertions to include sandbox artifacts.

## Concrete Steps

From repository root:

    cargo fmt --all
    cargo test --all-targets

User-level check:

    cargo run -p openoman-cli -- --config ./config.toml run <job_id>

Expected observable outputs after implementation include `sandbox.patch`, `sandbox.report`, and `sandbox.logs` entries under `openoman artifacts <job_id>`.

## Validation and Acceptance

Acceptance is met when:

1. Core tests demonstrate lifecycle behavior: runner starts, smoke process exits cleanly, artifacts are collected, and stop cleans up directories.
2. CLI run path calls the sandbox lifecycle and stores sandbox artifact records.
3. `openoman artifacts <job_id>` includes patch/report/log entries with non-zero output sizes.

## Idempotence and Recovery

Runner directories are scoped by job/attempt handle and removed in `stop`, so repeated test runs do not accumulate state. If a run fails mid-lifecycle, rerunning the same CLI command after resetting job state can recreate fresh runner directories.

## Artifacts and Notes

Expected sandbox artifact refs:

    sandbox.patch
    sandbox.report
    sandbox.logs

## Interfaces and Dependencies

`openoman-core` will expose:

    pub trait SandboxRunner {
        fn start(&mut self, spec: AttemptSpec) -> Result<SandboxHandle, SandboxError>;
        fn wait(&mut self, handle: &SandboxHandle) -> Result<SandboxExitStatus, SandboxError>;
        fn collect_artifacts(&self, handle: &SandboxHandle) -> Result<CollectedArtifacts, SandboxError>;
        fn stop(&mut self, handle: &SandboxHandle) -> Result<(), SandboxError>;
    }

The CLI run path will instantiate `FirecrackerRunner` and translate `CollectedArtifacts` into `NewArtifactRecord` rows.

Revision note (2026-03-01): Created this ExecPlan for Epic 5 implementation because the task crosses adapter design, runtime orchestration, persistence integration, and test coverage.

Revision note (2026-03-01): Updated this ExecPlan after implementation to reflect completed milestones, test outcomes, and discovered tar/path behavior.
