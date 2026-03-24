# Firecracker host-proxy sudo keepalive

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `docs/PLANS.md`.

## Purpose / Big Picture

After this change, a long-running `openoman run <job_id>` that uses Firecracker `host-proxy` networking should not fail at the very end just because the initial `sudo -v` timestamp expired before tap teardown. The user-visible effect is that a job whose sandbox work succeeded can still complete cleanup successfully without needing another interactive sudo prompt minutes later.

## Progress

- [x] (2026-03-17 12:42Z) Confirmed the failing path: `run_job()` primes `sudo` once before the attempt, while Firecracker teardown later invokes `sudo -n ... teardown`, so long runs can fail cleanup if the sudo timestamp expires.
- [x] (2026-03-17 12:54Z) Implemented a host-side `NetworkPrivilegeGuard` in `crates/cli/src/config.rs` that primes sudo once and refreshes the timestamp in a background thread for the duration of `run_job()`.
- [x] (2026-03-17 12:58Z) Updated `crates/cli/src/service.rs` so `OpenomanService::run_job()` keeps the guard alive across the full job lifecycle.
- [x] (2026-03-17 13:05Z) Added focused regression coverage with a fake sudo binary and ran `cargo test -p openoman-cli ensure_network_privileges_guard`, `cargo test -p openoman-cli config`, and `cargo fmt --all`.

## Surprises & Discoveries

- Observation: The sandbox attempt can succeed and still leave the overall job in `failed`.
  Evidence: `RunJobUseCase` treats `runner.stop()` errors as fatal after artifacts are already collected.

- Observation: The teardown failure is caused by host privilege expiry, not by guest Docker networking itself.
  Evidence: the failing runtime log ends with `network helper failed: exit=Some(1) stderr=sudo: a password is required`.

## Decision Log

- Decision: Keep the change in the CLI host layer rather than in Firecracker runtime code.
  Rationale: the problem is the lifetime of host sudo credentials, and the only place that currently primes those credentials is `crates/cli/src/config.rs`.
  Date/Author: 2026-03-17 / Codex

- Decision: Hold credentials alive only for the duration of `run_job()`.
  Rationale: this fixes the observed cleanup failure with minimal scope and preserves the current single-command lifecycle.
  Date/Author: 2026-03-17 / Codex

- Decision: Refresh the sudo timestamp with a lightweight background thread that runs `sudo -n -v` on a fixed cadence.
  Rationale: this matches the existing sudo contract, avoids touching Firecracker helper semantics, and is easy to stop automatically via Drop when `run_job()` returns.
  Date/Author: 2026-03-17 / Codex

## Outcomes & Retrospective

The keepalive mechanism is now implemented in the CLI host layer. `run_job()` acquires sudo once, then keeps the timestamp warm until the command exits, which directly addresses the observed teardown failure mode for long host-proxy jobs.

The main remaining gap is a live end-to-end repro against a real long-running Firecracker host-proxy job on this machine. Focused unit coverage is in place, but the final proof still depends on running a real job that lasts longer than the local sudo timeout and observing successful tap teardown.

## Context and Orientation

`crates/cli/src/service.rs` is the CLI orchestration layer. Its `OpenomanService::run_job()` method now calls `ensure_network_privileges_guard()` before loading and executing the job. `crates/cli/src/config.rs` contains that helper and already knows when Firecracker networking is in `host-proxy` mode with `privilege_mode = "sudo"`.

The Firecracker host network helper itself lives behind hidden CLI subcommands in `crates/cli/src/internal.rs`. Setup and teardown both run via `sudo -n <current_exe> internal firecracker-net ...`. The sandbox runner in `crates/core/src/execution/firecracker.rs` treats teardown failures as fatal, so if the host sudo timestamp expires between setup and teardown the overall job becomes `failed` even though the guest work is already done.

## Plan of Work

Add a small guard type in `crates/cli/src/config.rs` that performs the existing interactive `sudo -v` check once and, when needed, spawns a background keepalive loop that periodically runs `sudo -n -v`. Return that guard from `ensure_network_privileges_guard()` and keep it alive in `OpenomanService::run_job()` for the full duration of the command.

Make the implementation testable by factoring the sudo command path and refresh interval into an internal helper. The production path should still use `sudo` and a conservative refresh cadence that is comfortably shorter than the usual sudo timeout. Tests should use a temporary fake sudo script so they can prove that the initial prime happens, keepalive refreshes happen, and non-host-proxy configurations do not spawn the helper.

## Concrete Steps

From the repository root:

1. Edit `crates/cli/src/config.rs` to replace the bare `sudo -v` helper with a returned keepalive guard.
2. Edit `crates/cli/src/service.rs` so `run_job()` keeps the returned guard alive until the method exits.
3. Add unit tests in `crates/cli/src/config.rs` for the keepalive helper using a temporary fake sudo binary.
4. Run focused tests:

       cargo test -p openoman-cli ensure_network_privileges

   Then run a broader config/service test slice:

       cargo test -p openoman-cli config

## Validation and Acceptance

Acceptance is:

- `OpenomanService::run_job()` still prompts interactively for `sudo -v` only once at the start when host-proxy sudo mode is enabled.
- The returned guard refreshes the sudo timestamp non-interactively during the run.
- Focused CLI tests prove the keepalive behavior and pass locally.
- A long-running host-proxy job should no longer fail cleanup solely because `sudo -n` sees an expired timestamp.

## Idempotence and Recovery

The keepalive loop must be best-effort and safe to drop. Re-running the same tests should not require manual cleanup. The guard must stop refreshing once `run_job()` exits so background threads do not accumulate.

## Artifacts and Notes

Representative failing host log excerpt:

    tearing down tap device oomtap1
    running network helper: sudo -n /path/to/openoman internal firecracker-net teardown --tap-name oomtap1
    network helper failed: exit=Some(1) stderr=sudo: a password is required

## Interfaces and Dependencies

The main new interface is a small RAII-style guard returned by `ensure_network_privileges_guard()` in `crates/cli/src/config.rs`. `OpenomanService::run_job()` should bind that guard to a local variable so Drop semantics stop the keepalive thread automatically when the run completes.

Change note: Created on 2026-03-17 to track the host-side sudo timestamp expiry that can fail Firecracker host-proxy teardown after a successful sandbox attempt.
Change note: Updated on 2026-03-17 after implementing the keepalive guard in the CLI host layer and validating it with focused tests.
