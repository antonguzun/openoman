# Repo-specific post-clone command before sandbox copy

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `docs/PLANS.md`.

## Purpose / Big Picture

After this change, a repository alias in `config.toml` can declare an optional host-side command that runs after the trusted clone is created and checked out, but before that trusted clone is copied into the sandbox workspace. The user-visible effect is that repository-specific preparation such as pulling nested repositories or updating submodules can happen automatically without baking repo-specific behavior into `openoman`.

## Progress

- [x] (2026-03-17 13:17Z) Traced the workspace preparation flow: `GitAdapter::prepare_workspace_with_env_overlay_and_clone_token()` clones into `trusted-clone`, checks out the requested revision, and immediately copies that tree into `sandbox-workspace`.
- [x] (2026-03-17 13:28Z) Added `post_clone_command` to `[[git.repos]]`, stored it in repo runtime config, and exposed it through `AppConfig::post_clone_command_for_alias()`.
- [x] (2026-03-17 13:33Z) Threaded the optional command through `OpenomanService`, `RunJobUseCase`, and `GitAdapter`, and executed it in the trusted clone after checkout and before sandbox copy.
- [x] (2026-03-17 13:40Z) Added focused regression tests for config loading and trusted-clone command execution and ran `cargo test -p openoman-core git`, `cargo test -p openoman-cli config`, and `cargo fmt --all`.

## Surprises & Discoveries

- Observation: the clone/checkout/copy sequence already lives entirely in `crates/core/src/git.rs`.
  Evidence: `prepare_workspace_with_env_overlay_and_clone_token()` performs clone, checkout, `copy_tree()`, env overlay injection, and sandbox git sanitization in one function.

- Observation: the existing configuration model already carries repo-specific runtime metadata from `[[git.repos]]` to `OpenomanService`.
  Evidence: `RepoRuntimeConfig` stores `env_repo_name`, publish settings, and account binding, and `AppConfig` exposes repo-specific accessors such as `env_overlay_dir_for_alias()` and `resolve_clone_token_for_job()`.

## Decision Log

- Decision: make the hook repo-specific under `[[git.repos]]`, not global.
  Rationale: the user explicitly wants repository-level behavior, and nested-repository bootstrap commands are highly repo-dependent.
  Date/Author: 2026-03-17 / Codex

- Decision: store the hook as a single shell command string.
  Rationale: the requirement is for an arbitrary command, and a shell string is the smallest config surface that supports common bootstrap flows such as `git submodule update --init --recursive`.
  Date/Author: 2026-03-17 / Codex

- Decision: run the command after clone and checkout, but before sandbox copy.
  Rationale: repository preparation usually needs the requested revision checked out first, and the sandbox input should reflect the prepared trusted worktree.
  Date/Author: 2026-03-17 / Codex

- Decision: route the hook through a structured `PrepareWorkspaceOptions` value rather than adding another long helper name.
  Rationale: `GitAdapter` already had multiple optional preparation knobs. A single options struct keeps the core API readable while allowing the new hook to remain optional.
  Date/Author: 2026-03-17 / Codex

## Outcomes & Retrospective

The repo-specific hook is now implemented end to end. A repo alias can declare `post_clone_command`, `openoman run` resolves it by alias, and `GitAdapter` runs it in the trusted clone after checkout and before sandbox copy. This keeps the behavior repository-owned and optional, which matches the original goal of staying repo-agnostic while still supporting repo-specific bootstrap work.

The main residual tradeoff is that the hook is a shell string, so correctness and idempotence of the command remain the config author's responsibility. That is acceptable here because the feature is explicitly intended for trusted local operator configuration rather than untrusted user input.

## Context and Orientation

`crates/cli/src/config.rs` parses `config.toml` into runtime structures. `[[git.repos]]` entries already support alias-specific metadata such as `repo_ref`, `env_repo_name`, and publish settings. `crates/cli/src/service.rs` resolves repo-specific config for a job and constructs `RunJobUseCase`.

`crates/core/src/application/mod.rs` owns the top-level run flow. It asks `GitAdapter` to prepare the trusted and sandbox workspaces before starting the sandbox backend. `crates/core/src/git.rs` implements that preparation. The trusted clone is a host-side git worktree used for patch generation and publishing. The sandbox workspace is a copied and sanitized version that the agent mutates inside the sandbox.

The hook added by this plan must run only on the trusted host, inside the trusted clone directory, after `git checkout` of the requested revision and before `copy_tree()` populates the sandbox workspace.

## Plan of Work

First, extend the repo config model in `crates/cli/src/config.rs` with an optional `post_clone_command` string under `[[git.repos]]`. Normalize it like other optional strings, keep it `None` when omitted or blank, and add an accessor on `AppConfig` that resolves the command for a job by alias.

Second, thread that optional command through `OpenomanService::run_job()` and `RunJobUseCase`. Keep the field optional all the way down so raw repos and aliases without a command behave exactly as before.

Third, update `crates/core/src/git.rs` so workspace preparation can accept the optional command. After the current clone and checkout steps, execute `/bin/sh -lc <command>` with the trusted clone as the current working directory. If the command fails, surface it as the existing `GitError::CommandFailed` so the job fails early with a direct preparation error.

Finally, add focused tests. `crates/cli/src/config.rs` should prove that the repo command loads from config and is resolved by alias. `crates/core/src/git.rs` should prove that the command can materialize files in the trusted clone before sandbox copy and that a failing command aborts preparation.

## Concrete Steps

From the repository root:

1. Edit `crates/cli/src/config.rs` to parse and expose `[[git.repos]].post_clone_command`.
2. Edit `crates/cli/src/service.rs` and `crates/core/src/application/mod.rs` to pass the optional command into workspace preparation.
3. Edit `crates/core/src/git.rs` to run the optional command in the trusted clone after checkout and before `copy_tree()`.
4. Update `config.example.toml` and `README.md` to document the new repo-level hook.
5. Run focused tests:

       cargo test -p openoman-core git

   Then run the CLI config slice:

       cargo test -p openoman-cli config

## Validation and Acceptance

Acceptance is:

- a repo alias can define `post_clone_command = "..."` in `config.toml`;
- `openoman run` resolves that command only for the matching alias;
- the command runs in the trusted clone after checkout and before sandbox copy;
- files or directories created by the command are visible in the sandbox workspace;
- a non-zero command exit aborts workspace preparation with a clear command failure.

## Idempotence and Recovery

The hook is optional. Omitting it preserves the current behavior. A failing hook should stop preparation before the sandbox starts, which makes retry safe: rerunning the job recreates the workspace root from scratch before cloning. The command itself remains repo-owned behavior, so idempotence of the command is up to the config author and should be chosen accordingly.

## Artifacts and Notes

Target config example:

    [[git.repos]]
    alias = "repo_name"
    repo_ref = "git@github.com:username/repo_name.git"
    post_clone_command = "git submodule update --init --recursive"

## Interfaces and Dependencies

The new external interface is a string field under `[[git.repos]]` in `config.toml`. Internally, `RepoRuntimeConfig` in `crates/cli/src/config.rs` must retain that string, `AppConfig` must expose it by alias, `RunJobUseCase` must carry it as optional runtime data, and `GitAdapter` in `crates/core/src/git.rs` must accept and execute it with `/bin/sh -lc`.

Change note: Created on 2026-03-17 to track a repo-specific trusted-clone preparation hook requested for nested-repository bootstrap before sandbox copy.
Change note: Updated on 2026-03-17 after implementing the repo-level `post_clone_command` hook and validating it with focused Rust tests.
