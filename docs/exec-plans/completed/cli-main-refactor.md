# Refactor CLI entrypoint into focused modules

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [docs/PLANS.md](docs/PLANS.md).

## Purpose / Big Picture

After this change, the `openoman` binary entrypoint in `crates/cli/src/main.rs` will be short enough to read top-to-bottom in one pass. Command-line parsing, top-level command execution, configuration loading, and Firecracker networking helpers will live in separate files with names that match their responsibilities. The observable behavior should stay the same: `cargo test -p openoman-cli` still passes, and running the CLI still exercises the same commands and config validation paths as before.

## Progress

- [x] (2026-03-04 15:07Z) Reviewed `docs/PLANS.md`, `crates/cli/src/main.rs`, and current repo diffs to scope the refactor without overwriting unrelated in-flight changes.
- [x] (2026-03-04 15:14Z) Created `crates/cli/src/app.rs`, `crates/cli/src/cli.rs`, `crates/cli/src/config.rs`, and `crates/cli/src/internal.rs`, and reduced `crates/cli/src/main.rs` to a thin entrypoint.
- [x] (2026-03-04 15:14Z) Moved top-level command execution out of `run()` into dedicated command-dispatch functions in `crates/cli/src/app.rs`.
- [x] (2026-03-04 15:14Z) Moved config parsing/loading helpers and their unit tests into `crates/cli/src/config.rs`.
- [x] (2026-03-04 15:14Z) Ran `cargo fmt` and `cargo test -p openoman-cli`; both completed successfully.

## Surprises & Discoveries

- Observation: `crates/cli/src/main.rs` is currently 2,325 lines long and mixes clap types, app config loading, command execution, network plumbing, and tests.
  Evidence: `wc -l crates/cli/src/main.rs` returned `2325 crates/cli/src/main.rs`.
- Observation: The working tree already contains unrelated edits in `crates/core` and CLI tests, so the refactor must layer on top rather than resetting files.
  Evidence: `git status --short` shows modified and added files outside the CLI refactor scope.
- Observation: The extraction was behavior-preserving enough that the existing CLI unit and end-to-end tests passed without any semantic follow-up changes.
  Evidence: `cargo test -p openoman-cli` finished with `22` unit tests and `14` end-to-end tests passing.

## Decision Log

- Decision: Treat this request as a significant refactor and create an ExecPlan before editing.
  Rationale: The repo instructions require an ExecPlan for significant refactors, and this work changes file structure across the CLI crate.
  Date/Author: 2026-03-04 / Codex
- Decision: Preserve behavior first and focus the refactor on extraction, naming, and readability rather than semantic changes.
  Rationale: The user asked for readability and moving code into functions/files, not for new CLI behavior.
  Date/Author: 2026-03-04 / Codex
- Decision: Keep the top-level command dispatcher in `crates/cli/src/app.rs` and move the Firecracker helper commands into `crates/cli/src/internal.rs` rather than creating many tiny command files.
  Rationale: This keeps the binary entrypoint small while still giving command execution and helper code dedicated homes with clear responsibilities.
  Date/Author: 2026-03-04 / Codex

## Outcomes & Retrospective

The CLI entrypoint is now split by responsibility. `crates/cli/src/main.rs` only starts the program and exits with the correct status code, `crates/cli/src/app.rs` owns normal command execution, `crates/cli/src/cli.rs` owns clap definitions, `crates/cli/src/config.rs` owns config parsing and helper logic, and `crates/cli/src/internal.rs` owns internal Firecracker networking commands. This meets the original goal of making the run path readable without changing CLI behavior.

Validation matched the original purpose: `cargo test -p openoman-cli` passed after the extraction, so the refactor stayed observationally equivalent for the covered unit and end-to-end flows.

## Context and Orientation

The binary crate lives in `crates/cli`. Today, `crates/cli/src/main.rs` defines the clap command types, the top-level `run()` function, config file structs, config normalization helpers, Cursor and GitHub auth helpers, Firecracker host-network setup helpers, and a large unit test module. The `openoman_core` crate already owns the reusable execution logic such as `RunJobUseCase` and the execution backend types in `crates/core/src/application` and `crates/core/src/execution`.

For this refactor, "entrypoint" means the code that starts in `fn main()` and hands control to the rest of the program. "Command dispatch" means matching the parsed clap command and calling the code that implements that command. "Helpers" means local utility functions that do not need to stay in `main.rs`, such as config parsing and Firecracker network shell helpers.

## Plan of Work

Create a small module tree under `crates/cli/src` so responsibilities are discoverable by file name. Keep `crates/cli/src/main.rs` limited to module declarations, calling `run()`, and process exit handling. Move the clap structs and enums into a `cli` module. Move the top-level command execution code into an `app` or `commands` module whose public API is a single `run()` function.

Move config file structs, runtime config structs, parsing helpers, path expansion helpers, and the config-related tests into a `config` module. Keep the same field names and validation behavior so the existing tests remain valid with minimal adjustments to imports. Move internal Firecracker network commands and their shell helpers into an `internal` module so the top-level app code can delegate to them without carrying the implementation details inline.

Keep cross-module APIs narrow. The top-level dispatcher should receive parsed CLI values and the loaded app config, then call small functions such as `run_submit`, `run_run`, `run_status`, `run_logs`, `run_artifacts`, and `run_result`. Where constants like log and patch size limits are needed, define them near the dispatch code instead of leaving them in `main.rs`.

## Concrete Steps

From the repository root `/home/antonguzun/Work/personal/openoman`:

1. Create the new source files under `crates/cli/src` and move code out of `main.rs` using `apply_patch` so file history stays reviewable.
2. Run:

       cargo test -p openoman-cli

3. If compilation or tests fail after extraction, update imports, visibility, and module boundaries, then rerun the same command until it passes.

Observed validation transcript:

    $ cargo fmt

    $ cargo test -p openoman-cli
       Finished `test` profile [unoptimized + debuginfo] target(s) in 1.56s
        Running unittests src/main.rs (...)
    test result: ok. 22 passed; 0 failed
        Running tests/cli_e2e.rs (...)
    test result: ok. 14 passed; 0 failed

## Validation and Acceptance

Acceptance is:

1. `crates/cli/src/main.rs` becomes a thin entrypoint that no longer contains the long `run()` match body or the helper implementation blocks.
2. The extracted modules compile together without changing the CLI’s externally visible behavior.
3. Running `cargo test -p openoman-cli` from the repository root passes. The existing config-loading unit tests and CLI end-to-end tests continue to exercise the same behavior after the file split.

All three acceptance criteria were met on 2026-03-04.

## Idempotence and Recovery

The refactor is source-only and can be repeated safely by editing the same files again. If a partial extraction leaves compilation broken, recovery is to keep moving the missing imports, types, or helper visibility into the target modules and rerun `cargo test -p openoman-cli` until the crate is green. Do not reset unrelated working tree changes in `crates/core` or test files.

## Artifacts and Notes

Important baseline observations gathered before editing:

    $ wc -l crates/cli/src/main.rs
    2325 crates/cli/src/main.rs

    $ git status --short
     M crates/cli/src/main.rs
     M crates/cli/tests/cli_e2e.rs
     M crates/core/src/lib.rs
     D crates/core/src/sandbox/backend.rs
     D crates/core/src/sandbox/firecracker.rs
     M crates/core/src/sandbox/mod.rs
    ?? crates/core/src/application/
    ?? crates/core/src/execution/

## Interfaces and Dependencies

At the end of the refactor, these interfaces should exist:

- `crates/cli/src/main.rs` defines `fn main()` and delegates to `crate::app::run()`.
- `crates/cli/src/cli.rs` defines the clap-facing `Cli`, `Commands`, `InternalCommands`, and `FirecrackerNetCommands` types.
- `crates/cli/src/app.rs` defines `pub fn run() -> Result<(), String>` and small command-specific helpers for each top-level CLI command.
- `crates/cli/src/config.rs` defines `AppConfig`, the deserialized file-config structs, and the config-loading helpers needed by `app.rs`.
- `crates/cli/src/internal.rs` defines the internal Firecracker networking command execution functions used by the dispatcher.

Revision note: created this ExecPlan at the start of implementation because the requested file extraction qualifies as a significant CLI refactor.
Revision note: updated after implementation to record the final module split and successful `cargo fmt` plus `cargo test -p openoman-cli` validation.
