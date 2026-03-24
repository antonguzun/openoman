# Add Default Guest Tools and Explicit Docker Registry Auth

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` is checked into this repository and this document must be maintained in accordance with it.

## Purpose / Big Picture

After this change, the default Firecracker guest image contains a minimal baseline toolchain that agent runs expect in common test workflows: `make`, Docker CLI, and a default system Python exposed as both `python3` and `python`. Operators no longer need to rebuild their per-run staging layout just to make those baseline commands exist inside the sandbox.

The change also adds an explicit, opt-in path for private Docker registry credentials. When configured, openoman stages a Docker config file into the guest as `/root/.docker/config.json`; when not configured, no Docker registry secrets enter the sandbox. The visible proof is that guest asset docs describe the new default tools, config loading accepts the new Docker auth settings, and Firecracker runtime tests show the Docker config being staged only when explicitly requested.

## Progress

- [x] (2026-03-16 13:47Z) Reviewed the current rootfs build script, guest init contract, Firecracker runtime staging, and config loading to ground the feature in repo reality.
- [x] (2026-03-16 13:50Z) Created this ExecPlan and locked the requested behavior: baseline tools live in rootfs; Docker auth enters the guest only when explicitly configured.
- [x] (2026-03-16 14:04Z) Extended Firecracker config/runtime types and staging to support explicit Docker auth from either a host file path or a host environment variable.
- [x] (2026-03-16 14:07Z) Updated guest init and default rootfs build so the guest installs Docker auth into `/root/.docker/config.json` when configured and the default image now provisions `make`, Docker CLI, `python3`, and `python`.
- [x] (2026-03-16 14:13Z) Updated `config.example.toml`, `README.md`, and `guest/README.md` to describe the new default rootfs tools and explicit Docker auth flow.
- [x] (2026-03-16 14:15Z) Ran `cargo fmt --all`, `cargo test -p openoman-cli config::tests`, `cargo test -p openoman-core execution::firecracker::tests`, `cargo test -p openoman-cli`, and `cargo test -p openoman-core`.

## Surprises & Discoveries

- Observation: the checked-in default rootfs build already installs more than just a shell; it provisions `node`, `npm`, `git`, `ripgrep`, `codex`, and `cursor-agent`.
  Evidence: `guest/build-rootfs.sh` defines a default `OPENOMAN_GUEST_SETUP_CMD` that runs `apt-get install ... nodejs npm ripgrep strace` plus `npm install -g @openai/codex` and the Cursor installer.

- Observation: the runtime already has a trusted path for staging one secret file into the guest and installing it at a tool-specific location.
  Evidence: `crates/core/src/execution/firecracker/runtime.rs` writes `agent-auth.json` into `openoman-config`, and `guest/openoman-init.sh` copies it into `/root/.codex/auth.json` or `/root/.config/cursor/auth.json`.

- Observation: current docs explicitly state that host secrets should not enter the sandbox by default.
  Evidence: `docs/design-docs/sandbox-and-safety.md` and `docs/design-docs/core-beliefs.md` both call out “no host secrets in the sandbox”.

- Observation: the existing Firecracker test suite already had the right seam for Docker auth coverage because it inspects the built ext4 image through `debugfs`.
  Evidence: `crates/core/src/execution/firecracker.rs::stage_runtime_tree_copies_agent_auth_file_when_configured` already verified staged config files inside the runtime image, and the new Docker auth test could follow the same pattern.

## Decision Log

- Decision: put `make`, Docker CLI, and Python into the default guest rootfs instead of solving the baseline tool problem with per-run host binary staging.
  Rationale: the user explicitly chose rootfs as the default delivery mechanism for these common tools, and that keeps guest behavior reproducible across runs and hosts.
  Date/Author: 2026-03-16 / Codex

- Decision: support Docker registry auth only through explicit config fields, never through implicit reuse of host `~/.docker/config.json`.
  Rationale: automatic reuse would silently move host registry credentials into an untrusted microVM and conflict with the repository’s stated safety model.
  Date/Author: 2026-03-16 / Codex

- Decision: install a `python -> python3` symlink in the guest rootfs.
  Rationale: the observed failing run called `python`, not `python3`, so the guest should satisfy that common expectation directly.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The implementation achieved the planned outcome. The default rootfs build now provisions `make`, Docker CLI, `python3`, and a `python` compatibility symlink in addition to the existing agent toolchain. Operators can also opt into private Docker registry auth through explicit Firecracker config fields that stage a Docker config into `/root/.docker/config.json` only when requested.

The trust boundary stayed aligned with the repository’s safety model. Docker auth is never imported automatically from the host; the sandbox receives registry credentials only when the operator explicitly configures either a host file path or an environment variable that contains the Docker config JSON. Validation covered focused config/runtime suites and broader `openoman-cli` and `openoman-core` test runs, all of which passed.

## Context and Orientation

The guest rootfs build lives in `guest/build-rootfs.sh`. It constructs an ext4 image by starting from a container image, running a setup command inside that container, exporting the filesystem tree, and injecting `guest/openoman-init.sh` as `/sbin/openoman-init`. The guest asset documentation lives in `guest/README.md`.

The operator-facing config loader lives in `crates/cli/src/config.rs`. It deserializes `[sandbox.firecracker]` and converts it into `FirecrackerBackendConfig`, which then flows into the core execution runtime.

The Firecracker runtime staging logic lives in `crates/core/src/execution/firecracker/runtime.rs`. It builds the per-run runtime tree under `runtime-tree/`, writes `agent.env`, optional staged auth files, and the package mount manifest, then turns that tree into the writable runtime ext4 image mounted inside the guest as `/mnt/runtime`.

The guest init contract lives in `guest/openoman-init.sh`. It mounts `/dev/vdb`, loads `/mnt/runtime/openoman-config/agent.env`, optionally installs staged auth material, configures networking, and finally launches the agent.

The configuration and runtime types that represent Firecracker settings live in `crates/cli/src/config.rs` and `crates/core/src/execution/backend.rs`. Tests for config loading live in `crates/cli/src/config.rs`; tests for Firecracker env rendering and runtime staging live in `crates/core/src/execution/firecracker.rs`.

## Plan of Work

First, extend the public Firecracker config shape with an explicit Docker auth input. The config loader in `crates/cli/src/config.rs` should accept either a host file path or a host environment variable whose value is the JSON contents of a Docker CLI config. These two options must be mutually exclusive, and absence of both means Docker auth is disabled.

Second, carry that explicit Docker auth input through the runtime types into the Firecracker runtime stager. The stager should materialize `docker-config.json` inside `runtime-tree/openoman-config/` only when auth is configured, and it should add a guest environment variable that tells `guest/openoman-init.sh` where the staged file lives.

Third, update `guest/openoman-init.sh` to install the staged Docker config into `/root/.docker/config.json` with `0600` permissions, analogous to the existing agent auth-file installation flow. The script should log only the install path and never print the file contents.

Fourth, update `guest/build-rootfs.sh` so the default `OPENOMAN_GUEST_SETUP_CMD` installs the requested baseline tools. The final image must expose working `make`, `docker`, `python3`, and `python` commands without requiring per-run package mounts.

Finally, update tests and docs. Config tests should cover the new mutually exclusive Docker auth options. Firecracker runtime tests should prove that Docker auth is staged only when configured and that the env file points at the correct guest path. Documentation should describe the new default rootfs toolchain and the explicit opt-in Docker auth flow.

## Concrete Steps

From repository root `/home/antonguzun/Work/personal/openoman`:

1. Edit `crates/cli/src/config.rs` to parse and validate explicit Docker auth settings under `[sandbox.firecracker]`.
2. Edit `crates/core/src/execution/backend.rs` and `crates/core/src/execution/firecracker/runtime.rs` to carry and stage Docker auth.
3. Edit `guest/openoman-init.sh` to install `/root/.docker/config.json` when the staged file is present.
4. Edit `guest/build-rootfs.sh` and `guest/README.md` to make `make`, Docker CLI, and Python part of the default guest image.
5. Update `config.example.toml` and `README.md` to document the new config shape and tool availability.
6. Extend unit tests in `crates/cli/src/config.rs` and `crates/core/src/execution/firecracker.rs`.
7. Run:

    cargo fmt --all
    cargo test -p openoman-cli config::tests
    cargo test -p openoman-core execution::firecracker::tests
    cargo test -p openoman-cli
    cargo test -p openoman-core

## Validation and Acceptance

Acceptance is met when all of the following are true:

- The default guest rootfs build script provisions `make`, Docker CLI, `python3`, and `python`.
- Config loading accepts exactly one of the explicit Docker auth inputs and rejects invalid combinations with clear errors.
- Firecracker runtime staging writes a Docker config into `openoman-config/` only when explicitly configured.
- Guest init installs `/root/.docker/config.json` with restrictive permissions and without logging secret contents.
- Existing runs without Docker auth config keep working and do not receive any Docker registry credentials.

## Idempotence and Recovery

Rebuilding the rootfs with `./guest/build-rootfs.sh ./guest/out` remains safe to repeat because the script recreates `rootfs.ext4` from scratch. Config validation changes are additive. If a Docker auth configuration is wrong, removing the config fields returns the system to the default no-registry-auth behavior without further cleanup.

## Artifacts and Notes

The completed version of this section should include concise evidence from:

- a config test proving the mutually exclusive Docker auth validation
- a Firecracker runtime test showing the staged `docker-config.json`
- the final validation commands with passing results

Representative paths the implementation should materialize when auth is enabled:

    /mnt/runtime/openoman-config/docker-config.json
    /root/.docker/config.json

## Interfaces and Dependencies

At completion, the following interfaces must exist or be updated:

- In `crates/cli/src/config.rs`, extend `FirecrackerConfig` with:

    docker_auth_config: Option<String>
    docker_auth_config_env: Option<String>

- In `crates/core/src/execution/backend.rs`, extend `FirecrackerBackendConfig` with a normalized explicit Docker auth representation, for example:

    pub docker_auth_config: Option<DockerAuthConfig>

    pub enum DockerAuthConfig {
        HostFile(PathBuf),
        InlineJson(String),
    }

- In `crates/core/src/execution/firecracker/runtime.rs`, write the staged Docker config to:

    runtime-tree/openoman-config/docker-config.json

  and export:

    OPENOMAN_DOCKER_AUTH_FILE='/mnt/runtime/openoman-config/docker-config.json'

- In `guest/openoman-init.sh`, install the staged file to:

    /root/.docker/config.json

Revision note (2026-03-16): Initial ExecPlan created before implementation because this feature crosses guest asset build defaults, sandbox config parsing, Firecracker runtime staging, guest init behavior, docs, and tests.
Revision note (2026-03-16): Updated progress and findings after implementing config/runtime staging, guest init/rootfs changes, and passing focused `openoman-cli config::tests` plus `openoman-core execution::firecracker::tests`.
Revision note (2026-03-16): Marked the ExecPlan complete after docs updates and passing broader `openoman-cli` and `openoman-core` validation.
