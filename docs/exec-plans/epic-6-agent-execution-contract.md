# Epic 6 — agent execution contract inside sandbox (MVP Codex wiring)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, a sandbox attempt is configured with an explicit agent contract (provider + binary + optional egress proxy settings) and the runner performs a Codex-oriented execution step before writing artifacts. A user can run `openoman run <job_id>` and then inspect stored logs/patch artifacts showing that agent contract wiring executed inside the sandbox lifecycle.

## Progress

- [x] (2026-03-01 12:45Z) Reviewed Epic 6 decomposition requirements and existing Epic 5 runner behavior.
- [x] (2026-03-01 12:54Z) Added core sandbox agent contract types and Codex-oriented execution script generation with proxy environment support.
- [x] (2026-03-01 13:01Z) Added CLI config parsing for `[agent]` settings and passed contract into sandbox `AttemptSpec`.
- [x] (2026-03-01 13:08Z) Updated config example and test expectations for Epic 6 outputs.
- [x] (2026-03-01 13:12Z) Ran `cargo fmt --all`, `cargo test --all-targets`, and attempted `npm run build` (expected ENOENT because repo has no NextJS package).
- [x] (2026-03-01 13:38Z) Addressed follow-up feedback by adding `instruction` at submit time and plumbing it through persistence and sandbox run execution.

## Surprises & Discoveries

- Observation: the repository has no NextJS app or `package.json`, so `npm run build` cannot execute despite the default instruction.
  Evidence: npm exits with `ENOENT` at `/workspace/openoman/package.json`.

## Decision Log

- Decision: keep Codex execution as an MVP contract verification step (`codex --version` if present + explicit fallback log) instead of requiring interactive Codex runs.
  Rationale: this provides deterministic behavior in CI/test environments while still enforcing the Epic 6 interface and log visibility requirements.
  Date/Author: 2026-03-01 / Codex

## Outcomes & Retrospective

Epic 6 MVP now adds an explicit agent runtime contract to the sandbox lifecycle and CLI config, including egress proxy propagation and an optional egress domain allowlist field, while preserving deterministic artifact generation and testability. Remaining future work is richer interactive command execution beyond contract-level invocation.

## Context and Orientation

The core sandbox adapter lives in `crates/core/src/sandbox.rs` and is invoked from CLI orchestration in `crates/cli/src/main.rs`. Epic 5 already performed a smoke run that generated synthetic artifacts. Epic 6 extends that flow by introducing explicit agent runtime inputs and wiring those inputs into the sandbox execution script.

## Plan of Work

Introduce new sandbox-level data structures (`AgentExecutionSpec`, `AgentProvider`) and include them in `AttemptSpec`. Update script generation in `FirecrackerRunner::start` to export proxy env vars when configured, try Codex version invocation when available, and write deterministic logs/report/patch text indicating Epic 6 execution.

In CLI config loading, parse a new optional `[agent]` section, validate provider selection (`codex`), and pass that runtime selection into `AttemptSpec` during `run`. Update config examples to document the new settings.

## Concrete Steps

From repository root:

    cargo fmt --all
    cargo test --all-targets
    npm run build

Expected: Rust format/tests pass. `npm run build` fails with ENOENT in this Rust-only repository.

## Validation and Acceptance

Acceptance is satisfied when:

1. Sandbox run path includes explicit agent contract fields in `AttemptSpec`.
2. Collected logs include evidence of agent wiring (instruction + optional proxy + Codex presence/fallback).
3. Patch/report/log artifacts are still collected and persisted via existing CLI run workflow.

## Idempotence and Recovery

The runner remains idempotent per attempt directory and can be re-run after failed attempts by rerunning `openoman run <job_id>` from queued state with fresh runtime dirs.

## Artifacts and Notes

Expected artifact refs remain:

    sandbox.patch
    sandbox.report
    sandbox.logs

## Interfaces and Dependencies

Core now exposes:

    pub struct AgentExecutionSpec {
        pub provider: AgentProvider,
        pub codex_bin: String,
        pub egress_proxy: Option<String>,
        pub egress_allowed_domains: Vec<String>,
    }

    pub enum AgentProvider {
        Codex,
    }

CLI config now supports:

    [agent]
    provider = "codex"
    codex_bin = "codex"
    egress_allowed_domains = ["api.openai.com"]
    egress_proxy_url = "http://proxy.internal:3128"

Revision note (2026-03-01): Created Epic 6 ExecPlan and updated it inline during implementation to record progress and decisions.


Revision note (2026-03-01): Updated plan after review feedback to require submit-time instructions and pass them to sandbox agent execution.
Revision note (2026-03-02): Updated the documented agent contract to include the optional `egress_allowed_domains` list used by the current guest runtime wiring.
