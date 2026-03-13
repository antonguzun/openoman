# Add Cursor Agent Provider Support

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [docs/PLANS.md](../PLANS.md).

## Purpose / Big Picture

After this change, `openoman run <job_id>` can execute either Codex or Cursor inside the sandbox using the same host-side workflow: prepare a workspace, run the agent in the guest, collect logs and a text report, and compute the canonical patch on the trusted host. Users can configure Cursor with provider-neutral `[agent]` keys instead of a Codex-specific schema, while existing Codex configurations keep working.

The visible proof is twofold. First, config loading accepts `provider = "cursor"` with a provider-neutral command field and either a direct API key or a host environment variable name for the API key. Second, the test suite can run a fake Cursor CLI through the same sandbox path used for Codex and observe modified workspace files plus stored `sandbox.report` and `sandbox.logs` artifacts.

## Progress

- [x] (2026-03-03 10:55Z) Explored the current CLI, core sandbox, guest init script, docs, and tests to locate every Codex-specific contract surface.
- [x] (2026-03-03 10:58Z) Confirmed the target Cursor CLI contract from official docs: binary `cursor-agent`, non-interactive `-p` mode, `--output-format`, and `CURSOR_API_KEY` authentication.
- [x] (2026-03-03 11:02Z) Wrote this ExecPlan before mutating tracked files, per repository instructions.
- [x] (2026-03-03 11:24Z) Refactored the agent config/runtime contract to provider-neutral fields while preserving Codex compatibility aliases.
- [x] (2026-03-03 11:31Z) Extended guest env staging and guest execution logic to support Cursor alongside Codex.
- [x] (2026-03-03 11:41Z) Added fake Cursor coverage in CLI/core tests and updated user-facing docs/examples.
- [x] (2026-03-03 11:47Z) Ran `cargo fmt --all`, `cargo test -p openoman-cli`, `cargo test -p openoman-core`, and `cargo test --all-targets`.

## Surprises & Discoveries

- Observation: the current agent contract is not only provider-specific in config parsing; the guest env file, runtime image staging, tests, and docs all hardcode Codex names.
  Evidence: `crates/cli/src/main.rs`, `crates/core/src/sandbox/mod.rs`, `crates/core/src/sandbox/firecracker.rs`, `guest/openoman-init.sh`, and `crates/cli/tests/cli_e2e.rs` each contain `codex_*` fields or `CODEX_BIN`-style environment keys.

- Observation: the guest-side Codex flow already maps cleanly onto a provider adapter boundary because artifact collection happens outside the agent command.
  Evidence: `guest/openoman-init.sh` is responsible only for running the agent and writing `report.txt`; canonical patch generation happens later in trusted host code.

- Observation: the CLI end-to-end suite does not execute the real guest init script when it uses the fake Firecracker backend, so Cursor coverage needed a fake Firecracker that reads `agent.env` rather than only a fake Cursor binary.
  Evidence: the original `crates/cli/tests/cli_e2e.rs` fake Firecracker path wrote `report.txt` and `logs.txt` directly without consuming the staged agent contract, which would have left `api_key_env` and provider-specific env rendering untested.

## Decision Log

- Decision: adopt provider-neutral `[agent]` keys (`bin`, `auth_file`, `api_key`, `api_key_env`) as the preferred public config surface while keeping `codex_bin` and `codex_auth_file` as Codex-only compatibility aliases.
  Rationale: the user explicitly asked to use the same interface shape across providers, and leaving Codex field names as the shared contract would make the Cursor configuration permanently misleading.
  Date/Author: 2026-03-03 / Codex

- Decision: support Cursor authentication through a host environment variable name that resolves to `CURSOR_API_KEY` at run time.
  Rationale: the official Cursor CLI supports API key auth for non-interactive usage; this avoids guessing at local browser credential formats and keeps secret handling explicit.
  Date/Author: 2026-03-03 / Codex

- Decision: keep the artifact contract unchanged.
  Rationale: both providers can be adapted to the existing `sandbox.logs`, `sandbox.report`, and canonical patch pipeline, so the rest of the system does not need provider-specific branches.
  Date/Author: 2026-03-03 / Codex

- Decision: invoke Cursor in print mode with `-f` in addition to `-p --output-format text`.
  Rationale: the host expects non-interactive sandbox execution that does not pause for approval prompts; `-f` makes the Cursor guest branch align with Codex's fully automated behavior.
  Date/Author: 2026-03-03 / Codex

- Decision: support a direct `agent.api_key` value for Cursor in addition to `agent.api_key_env`, with the two options treated as mutually exclusive.
  Rationale: the user needs to place the Cursor key directly in config rather than referencing a host environment variable, but retaining the env-based path keeps the earlier workflow available.
  Date/Author: 2026-03-03 / Codex

## Outcomes & Retrospective

The implementation achieved the planned outcome. `openoman` now accepts `provider = "cursor"` in the `[agent]` config block, prefers provider-neutral keys (`bin`, `auth_file`, `api_key`, `api_key_env`), preserves `codex_bin` and `codex_auth_file` as Codex-only compatibility aliases, and passes a provider-neutral agent contract into the sandbox.

The guest runtime now exports `AGENT_BIN`, uses `OPENOMAN_AGENT_AUTH_FILE` for staged auth files, injects `CURSOR_API_KEY` when needed, and supports both a Codex execution branch and a Cursor print-mode branch. The default guest rootfs build now installs both `codex` and `cursor-agent`, and the user-facing docs show how to configure each provider.

Validation matched the purpose section. `cargo test -p openoman-cli`, `cargo test -p openoman-core`, and `cargo test --all-targets` all passed after adding Cursor config tests, Cursor env-rendering tests, inline-key coverage, env-based fallback coverage, and CLI end-to-end coverage that proves a fake Cursor run modifies the workspace and stores `sandbox.report` and `sandbox.logs`.

## Context and Orientation

The host CLI config loader lives in `crates/cli/src/main.rs`. It deserializes `[agent]` settings, validates them, and places them into `AttemptSpec` when `openoman run <job_id>` starts a sandbox attempt. Today that contract is Codex-specific: `AgentRuntimeConfig` stores `codex_bin` and `codex_auth_file`, and provider validation accepts only `codex`.

The core sandbox agent contract lives in `crates/core/src/sandbox/mod.rs`. `AgentExecutionSpec` is passed into the Firecracker backend and currently contains `provider`, `codex_bin`, `codex_auth_file`, `egress_proxy`, and `egress_allowed_domains`.

The Firecracker-specific runtime staging lives in `crates/core/src/sandbox/firecracker.rs`. Its `render_agent_env` helper writes the guest environment file, currently with `AGENT_PROVIDER`, `CODEX_BIN`, and `OPENOMAN_CODEX_AUTH_FILE`. The same file stages an optional auth file into the runtime image and contains unit tests for env rendering and runtime staging.

The guest boot and agent execution contract lives in `guest/openoman-init.sh`. That script mounts the runtime disk, loads `agent.env`, configures guest networking when enabled, and runs the selected agent. At the start of this work, the script has only a `codex)` case and directly executes `codex exec`.

The CLI end-to-end tests live in `crates/cli/tests/cli_e2e.rs`. They currently use a fake Codex shell script and fake Firecracker backend to verify that the sandbox pipeline stores logs, report output, and patch artifacts. These tests are the safest place to prove Cursor support without depending on a real external Cursor installation.

## Plan of Work

First, update the public and internal agent contract to remove Codex-specific field names. In `crates/cli/src/main.rs`, extend `AgentConfig` to parse neutral keys plus legacy Codex aliases, resolve defaults based on the selected provider, and add validation that rejects provider-incompatible fields. `AgentRuntimeConfig` should carry neutral fields only. During `run`, resolve the configured API key environment variable for Cursor and place the resulting secret into the sandbox attempt.

Second, update the core sandbox contract in `crates/core/src/sandbox/mod.rs` and `crates/core/src/sandbox/firecracker.rs` to use neutral names such as `bin`, `auth_file`, and an optional provider-specific environment payload for Cursor. Continue staging a file for Codex auth when configured, but pass the Cursor API key directly through the generated `agent.env`.

Third, update `guest/openoman-init.sh` so the top-level environment keys are provider-neutral and the `case` statement handles both `codex` and `cursor`. The Codex branch should preserve the current behavior. The Cursor branch should run `cursor-agent --version` as a probe and then execute `cursor-agent -p --output-format text "$instruction"` inside the workspace while redirecting stdout to `report.txt`.

Fourth, update tests and docs. Extend CLI and core test helpers with a fake Cursor script that requires `CURSOR_API_KEY`, edits the workspace in place, and prints a short report. Update `config.example.toml`, `README.md`, and `guest/README.md` to describe the new neutral config surface, Codex compatibility aliases, and the Cursor auth paths (`api_key` direct, `api_key_env` fallback).

## Concrete Steps

From the repository root:

1. Edit `crates/cli/src/main.rs` and `crates/core/src/sandbox/mod.rs` to introduce provider-neutral agent fields and Cursor provider parsing.
2. Edit `crates/core/src/sandbox/firecracker.rs` to stage the new environment and provider-specific secrets.
3. Edit `guest/openoman-init.sh` to add the Cursor execution branch.
4. Edit `crates/cli/tests/cli_e2e.rs` and the `crates/core/src/sandbox/firecracker.rs` tests to cover Cursor.
5. Edit `config.example.toml`, `README.md`, and `guest/README.md` to document the new interface.
6. Run:

    cargo fmt --all
    cargo test -p openoman-cli
    cargo test -p openoman-core
    cargo test --all-targets

Expected successful validation includes fake Cursor tests that prove workspace edits and report/log artifact capture.

## Validation and Acceptance

Acceptance is met when all of the following are true:

`AppConfig::load` accepts a Cursor config that uses neutral `[agent]` keys and rejects provider-incompatible combinations with clear errors.

`openoman run <job_id>` can execute a fake Cursor CLI through the existing sandbox path and produce a modified workspace, `sandbox.report`, and `sandbox.logs`.

Existing Codex configurations that rely on `codex_bin` and `codex_auth_file` still load and execute without behavioral regression.

The automated commands in `Concrete Steps` complete successfully.

## Idempotence and Recovery

These edits are additive and can be applied incrementally. If a test fails mid-way, rerun the focused test for the edited area before running the full suite. The only compatibility-sensitive part is config parsing; preserve legacy Codex aliases until the neutral interface is fully validated.

## Artifacts and Notes

The final version of this section should include short excerpts from:

- a fake Cursor CLI test showing `sandbox.report` contents
- a config-loading test proving Cursor validation behavior
- the final test commands with passing results

Representative outcomes from the completed work:

- `crates/cli/tests/cli_e2e.rs::submit_then_run_supports_cursor_provider` now observes `fake cursor completed: add empty line in readme` in `report.txt` and `agent provider: cursor` plus `cursor api key present` in `logs.txt`.
- `crates/cli/src/main.rs::tests::resolve_agent_execution_spec_uses_configured_cursor_api_key` proves that an inline `agent.api_key` value is copied into the sandbox execution spec without touching the host environment.
- `crates/cli/src/main.rs::tests::resolve_agent_execution_spec_requires_cursor_host_env` still fails with `agent.api_key_env references missing environment variable ...` when the env-based fallback is configured but absent.
- Final validation commands:

    cargo fmt --all
    cargo test -p openoman-cli
    cargo test -p openoman-core
    cargo test --all-targets

## Interfaces and Dependencies

At the end of this work, the following interfaces must exist.

In `crates/core/src/sandbox/mod.rs`, define:

    pub struct AgentExecutionSpec {
        pub provider: AgentProvider,
        pub bin: String,
        pub auth_file: Option<PathBuf>,
        pub api_key: Option<String>,
        pub egress_proxy: Option<String>,
        pub egress_allowed_domains: Vec<String>,
    }

and:

    pub enum AgentProvider {
        Codex,
        Cursor,
    }

In `crates/cli/src/main.rs`, define runtime parsing semantics equivalent to:

    struct AgentRuntimeConfig {
        provider: AgentProvider,
        bin: String,
        auth_file: Option<PathBuf>,
        api_key: Option<String>,
        api_key_env: Option<String>,
        egress_proxy: Option<String>,
        egress_allowed_domains: Vec<String>,
    }

In `guest/openoman-init.sh`, the guest environment contract must include:

    AGENT_PROVIDER=<codex|cursor>
    AGENT_BIN=<resolved command path>
    OPENOMAN_AGENT_AUTH_FILE=<runtime auth file path, Codex only>
    CURSOR_API_KEY=<resolved API key, Cursor only>

Revision note (2026-03-03): Initial plan added at implementation start because this feature is a significant multi-file refactor and the repository requires an ExecPlan for that class of work.
