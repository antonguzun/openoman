# Add Claude Code Agent Provider Support

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [docs/PLANS.md](../../PLANS.md).

## Purpose / Big Picture

After this change, an operator can write `provider = "claude"` in the `[agent]` block of their config and `openoman run <job_id>` executes Anthropic's Claude Code CLI inside the sandbox, exactly where Codex or Cursor would otherwise run. Everything around the agent is unchanged: the host clones the repository, snapshots a workspace into the guest, collects `report.txt` and `logs.txt`, computes the canonical patch on the trusted host, and publishes a branch and pull request. The agent still never receives Git credentials.

The reason this matters is that Claude Code is authenticated by a subscription OAuth token rather than a per-token API key, so teams that already pay for a Claude subscription can use it for sandboxed code changes without buying access to a second vendor.

The visible proof is twofold. First, config loading accepts `provider = "claude"` with the same provider-neutral `[agent]` keys the other two providers use, and rejects Codex-only compatibility fields with a clear message. Second, the test suite runs a fake Claude CLI through the same sandbox path used for Codex and Cursor and observes a modified workspace file, a stored `sandbox.report` containing the agent's output, and a `sandbox.logs` artifact that proves the OAuth token never appeared on the command line.

## Progress

- [x] (2026-08-17) Read the agent adapter contract, both existing adapters, the guest init script, the rootfs build, and the CLI end-to-end tests to find every provider-specific surface in the current tree.
- [x] (2026-08-17) Confirmed the Claude Code CLI contract against a real 2.1.197 installation: `-p` for non-interactive runs, `--output-format text`, `--model`, `--dangerously-skip-permissions`, `--version` as a probe, and `CLAUDE_CODE_OAUTH_TOKEN` for subscription authentication.
- [x] (2026-08-17) Verified empirically that Claude Code refuses `--dangerously-skip-permissions` when running as uid 0 unless `IS_SANDBOX=1` is set, and that setting it lets the process past that check.
- [x] (2026-08-17) Implemented `ClaudeAdapter` in `crates/core/src/agents/mod.rs` and registered it in `AgentAdapterRegistry::with_defaults`.
- [x] (2026-08-17) Widened the two `CodexAdapter` rejection messages that claimed `model`, `api_key` and `api_key_env` were Cursor-only, keeping the substring `cursor` because an existing CLI config test asserts on it.
- [x] (2026-08-17) Added six unit tests in `crates/core/src/agents/mod.rs` and a fake Claude CLI plus an end-to-end test in `crates/cli/tests/cli_e2e.rs`.
- [x] (2026-08-17) Added the Claude Code install to `guest/build-rootfs.sh` and documented the provider in `README.md` and `config.example.toml`.
- [x] (2026-08-17) Ran `cargo fmt --all -- --check` and `cargo test -p openoman-core`; the six new unit tests pass and the core suite goes from 73 to 79 passing tests.
- [ ] Run the full suite and `cargo clippy --all-targets --all-features -- -D warnings` on a machine whose environment matches CI (see `Surprises & Discoveries` for why the development host cannot).
- [ ] Build a guest image with `./guest/build-assets.sh` and run one real job end to end against a throwaway repository.

## Surprises & Discoveries

- Observation: Claude Code hard-refuses `--dangerously-skip-permissions` when the process runs as root, and the guest runs the agent as uid 0 with `HOME=/root`. Without a mitigation, every Claude attempt would fail before the agent did any work.
  Evidence: running `claude -p --dangerously-skip-permissions "Reply with exactly: OK"` as root prints `--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons`, while the same command with `IS_SANDBOX=1` proceeds past that check. Strings in the shipped binary show the guard alongside `IS_SANDBOX` and `CLAUDE_CODE_BUBBLEWRAP`, which is the CLI's own bubblewrap-based sandbox mechanism.

- Observation: the guest wraps the agent version probe in `timeout -k 1 10`, and a non-zero exit aborts the attempt before the agent runs at all.
  Evidence: `guest/openoman-init.sh` runs `timeout -k 1 10 "$@"` for the probe and sets `agent_status` from its exit code, then skips the agent when that status is non-zero. In a guest whose only egress is an exact-hostname CONNECT allowlist, a startup auto-update or telemetry request to a host that is not allowlisted can hang past that deadline.

- Observation: the guest needs no new branch for a third provider, which contradicts what the earlier Cursor ExecPlan describes.
  Evidence: `guest/openoman-init.sh` no longer contains a `case "$AGENT_PROVIDER"` statement. Since the launch-plan refactor it reconstructs the command generically from `OPENOMAN_AGENT_BIN`, `OPENOMAN_AGENT_ARG_COUNT` and `OPENOMAN_AGENT_ARG_NNN`, and branches only on `OPENOMAN_AGENT_REPORT_MODE`. That file is untouched by this work.

- Observation: an environment variable cannot be removed on the way into the guest, only added.
  Evidence: `AgentLaunchPlan.env` is a `Vec<(String, String)>` rendered as `export KEY='VALUE'` lines. This matters because Claude Code prefers `ANTHROPIC_API_KEY` over the OAuth token when both are present. It turns out no mitigation is needed: the guest is a fresh microVM whose environment is built solely from the generated `agent.env`, and the process backend calls `command.env_clear()` before adding an explicit allowlist, so a host `ANTHROPIC_API_KEY` cannot reach the agent by either path.

- Observation: the development host used for this work cannot produce a clean baseline for the full suite, so "all tests pass" could not be observed locally.
  Evidence: on that host five `crates/cli/tests/cli_e2e.rs` tests and one Firecracker test in `crates/core` fail identically before and after this change, and `cargo clippy -- -D warnings` reports errors in files this change does not touch, because a newer Rust release added the `manual_is_multiple_of` lint. The new Claude end-to-end test fails on the same assertion as the pre-existing Cursor test, which is the signal that the cause is environmental. CI runs on `ubuntu-latest`, where these are expected to pass.

## Decision Log

- Decision: carry the Claude credential in the existing `agent.api_key` and `agent.api_key_env` keys rather than adding a dedicated `oauth_token` key.
  Rationale: a new key would have to be threaded through the CLI config struct and its default literal, the runtime config input and output, two manual `Debug` implementations that redact secrets, and the execution spec. Reusing the established keys keeps the public config surface identical across all three providers and adds no new place where a secret could be logged by accident.
  Date/Author: 2026-08-17

- Decision: pass the token to the guest only through the launch plan environment, and set `redact_api_key_args: false`.
  Rationale: the guest's log redactor understands a single literal `--api-key` argument. Claude Code has no equivalent flag, so a token placed in argv would be written verbatim into the stored `sandbox.logs` artifact. A unit test asserts the token does not appear in the argument vector.
  Date/Author: 2026-08-17

- Decision: set `IS_SANDBOX=1` in the launch plan environment.
  Rationale: the guest runs the agent as uid 0, and Claude Code refuses `--dangerously-skip-permissions` under root unless it is told an external sandbox already exists. That is precisely the situation here: the agent runs inside a disposable microVM whose egress is restricted to an explicit hostname allowlist, so the isolation boundary is the VM, not the user id inside it. The variable asserts that boundary rather than creating one.
  Date/Author: 2026-08-17

- Decision (revised 2026-08-18): move `IS_SANDBOX=1` out of the launch plan and into the Firecracker backend's `agent.env` renderer.
  Rationale: `AgentLaunchPlan.env` is also applied verbatim by the process backend, which runs the agent directly on the host — there the variable would forge sandbox status and disable Claude Code's own refusal to run `--dangerously-skip-permissions` under root. Sandbox status is a backend fact, so the backend that actually provides the microVM asserts it, and every provider inside the VM sees it.
  Date/Author: 2026-08-18

- Decision: set `DISABLE_AUTOUPDATER=1` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`.
  Rationale: the version probe has a ten-second deadline and its failure aborts the attempt. Background requests to hosts outside the egress allowlist are the most likely way to exceed it, and neither auto-update nor telemetry is wanted in a disposable sandbox.
  Date/Author: 2026-08-17

- Decision: report through `AgentReportMode::Stdout` with `--output-format text` rather than `json`.
  Rationale: in stdout mode the agent's entire standard output becomes `report.txt`, which is surfaced to humans as the `sandbox.report` artifact. Emitting the JSON envelope there would make the artifact worse than the other two providers produce, and nothing in this codebase consumes the structured cost or turn-count fields.
  Date/Author: 2026-08-17

- Decision: require `api.anthropic.com` in `agent.egress_allowed_domains` when host-proxy networking is enabled.
  Rationale: this mirrors the existing Cursor rule for `api2.cursor.sh`. Allowlist matching is exact per hostname, so the domain must be listed verbatim; the whole-list `"*"` debug escape hatch continues to bypass the check.
  Date/Author: 2026-08-17

- Decision: run the agent as root in the guest like the other providers, instead of creating an unprivileged user.
  Rationale: switching users would require a provider-specific branch in the guest init script, which was deliberately made provider-neutral, and would break symmetry with Codex and Cursor. It would also complicate workspace permissions and Docker-in-guest access. Because the microVM is disposable and externally constrained, an unprivileged user inside it would add little. A defense-in-depth change of this kind is worth doing for all providers at once, not as part of adding one.
  Date/Author: 2026-08-17

## Outcomes & Retrospective

To be completed once the remaining two `Progress` items are done. The implementation itself matched the plan: no change was needed in `guest/openoman-init.sh`, no new configuration key was introduced, and the only edits outside the adapter were the two widened Codex error messages, the rootfs install line, the tests, and the documentation.

## Context and Orientation

An "agent provider" here means the command-line coding tool that runs inside the sandbox and edits the workspace. The repository supports two of them today, Codex and Cursor, and this work adds a third.

The provider abstraction lives in `crates/core/src/agents/mod.rs`. That file defines a private trait `AgentAdapter` with five methods, of which four must be implemented: `provider_id` returns the string used in config; `load_runtime_config` validates the `[agent]` block for that provider and fills in defaults; `resolve_execution_inputs` turns configuration into the secret and optional staged auth file that the sandbox will receive; `validate_host_proxy_egress` has a default implementation and is overridden when a provider needs a specific domain in the allowlist; and `build_launch_plan` produces the command line, environment and report mode for one run. Implementations are registered in `AgentAdapterRegistry::with_defaults`, and `parse_agent_provider` accepts any provider the registry knows, so registration is the only place a new provider becomes visible to config parsing.

Because the trait is private, a provider cannot be added from another crate. `ClaudeAdapter` is therefore a unit struct in that same file.

The struct `AgentLaunchPlan`, also in that file, is the contract between the host and the guest. It carries the binary, the argument vector, the working directory, a report mode of either `File` or `Stdout`, an optional path where a staged auth file should be installed relative to the guest home directory, a vector of environment variables, the arguments used to probe the agent's version, and a flag that tells the guest to redact a value following `--api-key` when logging the command.

The guest side is `guest/openoman-init.sh`. It boots as the first process in the microVM, sources a generated `agent.env`, reconstructs the agent command from numbered environment variables, runs a version probe under a ten-second timeout, then runs the agent and writes `report.txt` and `logs.txt`. It contains no provider-specific logic and is not modified by this work.

The guest image is produced by `guest/build-rootfs.sh`. A single long `GUEST_SETUP_CMD` variable installs system packages and the agent CLIs, then verifies each one with `command -v`. Those verification checks are the only automated protection against a broken install line, because no CI job builds the guest image.

The end-to-end tests are in `crates/cli/tests/cli_e2e.rs`. They do not boot a real microVM. Instead a fake Firecracker script reads the staged agent environment and writes artifacts directly into the runtime image, and a fake agent script stands in for the real CLI. The fake Firecracker script contains the one genuinely provider-specific branch in the test suite: a `case` on the provider name whose default arm fails with "unsupported provider".

## Plan of Work

First, add the adapter. In `crates/core/src/agents/mod.rs`, declare `struct ClaudeAdapter;` next to the other two, register it in `AgentAdapterRegistry::with_defaults`, and implement `AgentAdapter` for it. `load_runtime_config` defaults the binary to `claude`, accepts `model`, requires exactly one of `api_key` or `api_key_env`, and rejects the Codex compatibility fields with messages shaped like the Cursor ones. Add a small helper next to `resolve_cursor_execution_inputs` that resolves the token from either the inline value or the named host environment variable, with the same empty-value check.

Second, build the launch plan. Arguments are `-p`, `--dangerously-skip-permissions`, `--output-format text`, optionally `--model <id>`, then `--` and finally the instruction (the terminator keeps a dash-leading instruction positional). The environment carries `CLAUDE_CODE_OAUTH_TOKEN`, `DISABLE_AUTOUPDATER=1` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`; `IS_SANDBOX=1` is added by the Firecracker backend's `agent.env` renderer, not the plan (see the revised decision above). Report mode is `Stdout`, no auth file is staged, the version probe is `--version`, and `redact_api_key_args` is false because the token is never in argv.

Third, widen the two `CodexAdapter` messages that describe `model`, `api_key` and `api_key_env` as Cursor-only, since they are now also Claude keys. Keep the word `cursor` in the model message: `crates/cli/src/config.rs` asserts that the error contains it.

Fourth, cover it with tests. In `crates/core/src/agents/mod.rs` add a `claude_spec()` fixture and tests that assert the launch plan's provider id, report mode, arguments and environment; that the token stays out of argv; that `DISABLE_AUTOUPDATER` is present and `IS_SANDBOX` is absent from the plan (the Firecracker renderer test asserts it lands in `agent.env`); that Codex compatibility fields are rejected and a missing token is reported; that the binary defaults to `claude` and `model` is accepted; and that host-proxy validation demands `api.anthropic.com`. In `crates/cli/tests/cli_e2e.rs` add a fake Claude script that refuses to run without `CLAUDE_CODE_OAUTH_TOKEN`, a `claude_agent` config helper, a `claude` arm in the fake Firecracker `case`, and an end-to-end test asserting the report, the log lines, the sandbox flag and the absence of the token in the logs.

Fifth, install the CLI in the guest image and document the provider. Add the Claude Code installer to `GUEST_SETUP_CMD` and a matching `command -v claude` check. Update `README.md` and `config.example.toml`.

## Concrete Steps

From the repository root:

1. Edit `crates/core/src/agents/mod.rs` to add `ClaudeAdapter`, register it, add the token resolver, and widen the two Codex messages.
2. Edit `crates/cli/tests/cli_e2e.rs` to add the fake Claude CLI, the config helper, the fake Firecracker arm and the end-to-end test.
3. Edit `guest/build-rootfs.sh` to install Claude Code and verify it with `command -v claude`.
4. Edit `config.example.toml` and `README.md` to document the provider.
5. Run:

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test -p openoman-core
    cargo test -p openoman-cli
    cargo test --all-targets

Expected successful validation includes six new passing tests in `crates/core/src/agents/mod.rs` and a passing `submit_then_run_supports_claude_provider` in the CLI suite.

## Validation and Acceptance

Acceptance is met when all of the following are true.

Configuration loading accepts a Claude config that sets `provider = "claude"` with either an inline `api_key` or an `api_key_env` naming a host environment variable, and rejects `codex_bin`, `auth_file` and the combination of both key forms with messages that name the offending field.

`openoman run <job_id>` executes a fake Claude CLI through the existing sandbox path and produces a modified workspace file, a `sandbox.report` containing the agent's output, and a `sandbox.logs` artifact that contains `agent provider: claude` and does not contain the token.

Host-proxy validation fails with a message naming `api.anthropic.com` when that domain is missing from `agent.egress_allowed_domains` and Firecracker networking is set to `host-proxy`.

Existing Codex and Cursor configurations load and execute with no behavioral change.

The commands in `Concrete Steps` complete successfully on a machine matching CI.

## Idempotence and Recovery

Every edit is additive: a new adapter, new tests, one new install command, and documentation. Re-running the steps is safe. If a test fails partway, run the focused test for the area just edited before the full suite. The one place where care is needed is the pair of widened Codex error messages, because an existing CLI config test asserts on the substring `cursor`; if that test fails, restore the word rather than rewriting the test.

## Artifacts and Notes

Representative outcomes from the completed work:

- `crates/core/src/agents/mod.rs::tests::claude_adapter_builds_stdout_plan_with_token_in_env` observes `provider_id == "claude"`, `AgentReportMode::Stdout`, the instruction as the final argument, and `CLAUDE_CODE_OAUTH_TOKEN` in the plan environment.
- `crates/core/src/agents/mod.rs::tests::claude_launch_plan_keeps_token_out_of_argv` proves the token never enters the argument vector, which is what keeps it out of the stored logs.
- `crates/core/src/agents/mod.rs::tests::claude_launch_plan_marks_guest_as_sandbox_for_root_execution` pins the two environment variables whose absence would otherwise cause silent, hard-to-diagnose failures.
- Core suite before this change: 73 passing. After: 79 passing.

## Interfaces and Dependencies

At the end of this work the following must exist.

In `crates/core/src/agents/mod.rs`:

    struct ClaudeAdapter;

    impl AgentAdapter for ClaudeAdapter {
        fn provider_id(&self) -> &'static str { "claude" }
        fn load_runtime_config(&self, provider: &str, input: AgentRuntimeConfigInput)
            -> Result<AgentRuntimeConfig, String>;
        fn resolve_execution_inputs(&self, config: &AgentRuntimeConfig)
            -> Result<(Option<String>, Option<PathBuf>), String>;
        fn validate_host_proxy_egress(&self, egress_allowed_domains: &[String])
            -> Result<(), String>;
        fn build_launch_plan(&self, spec: &AgentExecutionSpec, context: AgentLaunchContext<'_>)
            -> Result<AgentLaunchPlan, ExecutionError>;
    }

registered by:

    registry.register(ClaudeAdapter);

The guest environment contract gains no new required keys. The launch plan environment for this provider is:

    CLAUDE_CODE_OAUTH_TOKEN=<subscription OAuth token from `claude setup-token`>
    IS_SANDBOX=1
    DISABLE_AUTOUPDATER=1
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1

The guest image must provide a `claude` executable on `PATH`.

Revision note (2026-08-17): Plan written alongside the implementation because the repository requires an ExecPlan for this class of work, and because two non-obvious runtime constraints, the root guard and the version-probe timeout, need to be recorded where the next person adding a provider will find them.
