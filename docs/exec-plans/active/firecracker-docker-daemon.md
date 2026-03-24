# Start Docker Daemon Inside Firecracker Guests

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` is checked into this repository and this document must be maintained in accordance with it.

## Purpose / Big Picture

After this change, a Firecracker guest can boot with a working Docker daemon running inside the microVM instead of only having the Docker CLI installed. That lets Docker-based test workflows such as `make test-local-up` progress past `Cannot connect to the Docker daemon at unix:///var/run/docker.sock` and reach the actual container startup path.

The visible proof is that a Firecracker run with the new config flag enabled exposes a live `/var/run/docker.sock` to guest processes and `docker info` succeeds inside the guest. A real Firecracker smoke test in `openoman-core` will verify that behavior end to end.

## Progress

- [x] (2026-03-16 16:06Z) Reviewed `guest/openoman-init.sh`, Firecracker config/runtime staging, current rootfs contents, and the failing job `job-1773669604844` to confirm the blocker is the missing daemon, not missing CLI binaries or registry auth.
- [x] (2026-03-16 16:09Z) Verified the pinned Firecracker kernel config already enables the features Docker needs most: namespaces, cgroups, `bridge`, `veth`, `iptables`, and `overlayfs`.
- [x] (2026-03-16 16:12Z) Created this ExecPlan and locked the first implementation shape: explicit Firecracker opt-in flag, guest-side `dockerd` startup in `openoman-init.sh`, and a real Firecracker smoke test that runs `docker info`.
- [x] (2026-03-16 16:24Z) Added `sandbox.firecracker.docker_daemon` to config loading, runtime types, docs, and examples, with a default of `false`.
- [x] (2026-03-16 16:34Z) Implemented guest-side `dockerd` startup behind the env marker, including `/run` tmpfs, `cgroup2` mount, daemon log capture, readiness polling, and a hard failure path when the daemon never becomes ready.
- [x] (2026-03-16 16:43Z) Added focused config/runtime tests plus a gated real Firecracker smoke test that boots a VM and requires `docker info` to succeed inside the guest.
- [x] (2026-03-16 16:58Z) Rebuilt the rootfs, fixed guest Docker networking startup by switching to legacy `iptables`, and reran the real Firecracker smoke test plus focused Rust test suites to green.

## Surprises & Discoveries

- Observation: the failing job already had Docker CLI and Docker registry auth staged correctly.
  Evidence: `workspaces/sandboxes/jobs/job-1773669604844/attempt-1/logs.txt` contains `installing staged docker auth file to /root/.docker/config.json`, `docker --version`, and then `ERROR: Cannot connect to the Docker daemon at unix:///var/run/docker.sock.`

- Observation: the pinned Firecracker guest kernel is already much closer to Docker-ready than expected.
  Evidence: `guest/out/vmlinux.config` contains `CONFIG_CGROUPS=y`, `CONFIG_NAMESPACES=y`, `CONFIG_VETH=y`, `CONFIG_BRIDGE=y`, `CONFIG_BRIDGE_NETFILTER=y`, and `CONFIG_OVERLAY_FS=y`.

- Observation: starting Docker daemon unconditionally would be a behavior change for every Firecracker run and could introduce avoidable startup cost or failures for non-Docker tasks.
  Evidence: current guest init does not mention Docker daemon startup at all, and the project only recently added Docker CLI plus registry auth as explicit operator-facing features.

- Observation: the first real guest `dockerd` boot failed on Docker network-controller initialization because Debian defaulted `iptables` to the nft backend while the pinned kernel is built around legacy xtables and does not enable `NF_TABLES`.
  Evidence: the failing real Firecracker smoke test logged `iptables: Failed to initialize nft: Protocol not supported` in `dockerd.log`, while `guest/out/vmlinux.config` shows `CONFIG_NETFILTER_XTABLES=y` and `# CONFIG_NF_TABLES is not set`.

## Decision Log

- Decision: make guest Docker daemon startup explicit through a new `sandbox.firecracker.docker_daemon` boolean instead of always starting it.
  Rationale: this solves the user’s Docker-test use case without silently changing startup behavior for every Firecracker job. It also lets operators leave Docker disabled when they only need the CLI or do not trust the extra resource overhead.
  Date/Author: 2026-03-16 / Codex

- Decision: start `dockerd` from `guest/openoman-init.sh` instead of trying to pre-launch it in the rootfs build or through host-side Firecracker wiring.
  Rationale: `openoman-init.sh` already owns guest boot sequencing, proxy environment export, auth-file installation, and agent startup. Docker daemon needs those guest-side conditions, especially proxy environment, before it starts.
  Date/Author: 2026-03-16 / Codex

- Decision: prove the feature with `docker info` in a real Firecracker smoke test before worrying about a full Docker Compose application stack.
  Rationale: `docker info` isolates the daemon-startup problem from registry pulls, repository-specific `.env_docker` files, and application-level compose logic. It is the smallest observable behavior that closes the blocker seen in the failing job.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The feature landed as an explicit Firecracker opt-in through `sandbox.firecracker.docker_daemon = true`. Host-side config parsing, runtime env staging, guest init, and docs all now agree on that interface, while the default remains unchanged for non-Docker jobs.

The guest now starts `dockerd` only when requested, writes daemon logs to `/mnt/runtime/openoman-output/dockerd.log`, waits for `docker info` readiness, and fails early with a readable log tail if startup breaks. The rootfs build also now pins the guest to legacy `iptables`, matching the current kernel config and avoiding Docker's nft-related network-controller failure during boot.

Validation reached the success bar defined at the top of this plan. The rebuilt rootfs passed:

- `cargo test -p openoman-cli config::tests`
- `cargo test -p openoman-core execution::firecracker::tests`
- `env PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" CARGO_HOME=/tmp/openoman-cargo-home OPENOMAN_FIRECRACKER_E2E=1 OPENOMAN_FIRECRACKER_BIN="$HOME/.local/bin/firecracker" OPENOMAN_FIRECRACKER_KERNEL="/home/antonguzun/Work/personal/openoman/guest/out/vmlinux" OPENOMAN_FIRECRACKER_ROOTFS="/home/antonguzun/Work/personal/openoman/guest/out/rootfs.ext4" cargo test -p openoman-core guest_docker_daemon_smoke_boots_real_firecracker_when_opted_in -- --nocapture`

That final real Firecracker smoke test passed and proved a guest can answer `docker info` when the operator opts into guest Docker daemon startup.

## Context and Orientation

The Firecracker backend is configured in `crates/cli/src/config.rs` and represented at runtime by `crates/core/src/execution/backend.rs::FirecrackerBackendConfig`. Any new Firecracker setting must be added in both places because the CLI layer parses TOML into a runtime struct that the execution layer consumes.

The per-run runtime image is assembled in `crates/core/src/execution/firecracker/runtime.rs`. That code writes `openoman-config/agent.env`, which `guest/openoman-init.sh` later sources during guest boot. This is the standard mechanism for passing per-run knobs from trusted host code into the guest.

The guest’s PID 1 process is `guest/openoman-init.sh`, injected into the root filesystem as `/sbin/openoman-init` by `guest/build-rootfs.sh`. PID 1 is the first userspace process inside the microVM, so this script is the right place to mount filesystems, configure networking, install staged auth files, start long-lived helper processes such as `dockerd`, and only then launch the agent workload.

The current rootfs already contains `docker`, `dockerd`, and `docker-compose` because `guest/build-rootfs.sh` installs the Debian `docker.io` and `docker-compose` packages. The current missing piece is daemon startup and readiness, not package installation.

The existing real Firecracker smoke test lives in `crates/core/src/execution/firecracker.rs::direct_runner_smoke_boots_real_firecracker_when_opted_in`. It boots a real VM only when `OPENOMAN_FIRECRACKER_E2E=1` is set and uses a staged fake `codex` binary to perform a tiny workload inside the guest. That makes it the safest place to add a second real-VM smoke test for guest Docker daemon startup.

## Plan of Work

First, extend the public Firecracker config shape with a boolean `docker_daemon` flag in `crates/cli/src/config.rs` and `crates/core/src/execution/backend.rs`. The default must remain `false`. `load_firecracker_config` should populate the runtime struct with that value, and docs should describe it as “start `dockerd` inside the guest.”

Second, pass that setting into the guest through `render_agent_env` in `crates/core/src/execution/firecracker/runtime.rs`. When enabled, `agent.env` should contain a simple marker such as `OPENOMAN_DOCKER_DAEMON_ENABLED='1'`. No extra runtime image files are required for the first version.

Third, update `guest/openoman-init.sh` so that after networking and proxy environment are configured, but before the agent is launched, it can optionally start the Docker daemon. The script should:

1. Mount `/run` as `tmpfs` early in boot because `dockerd` expects runtime state under `/run` and `/var/run`.
2. Mount `/sys/fs/cgroup` as `cgroup2` if it is not already mounted.
3. Start `dockerd` in the background with a dedicated log file under `/mnt/runtime/openoman-output/dockerd.log`.
4. Wait for readiness by retrying `docker info` for a bounded interval.
5. Fail the whole guest boot with a clear message and the tail of `dockerd.log` if readiness never arrives.

The startup command should prefer a stable configuration. The first version should try `overlay2` storage driver because the pinned kernel exposes `CONFIG_OVERLAY_FS=y`, and fall back to `vfs` if `overlay2` fails. This keeps the feature functional even if a future kernel or filesystem combination breaks overlay-based storage.

Fourth, add tests. Config tests in `crates/cli/src/config.rs` should prove `docker_daemon = true` loads correctly and defaults to `false`. Firecracker env-rendering tests in `crates/core/src/execution/firecracker.rs` should prove the guest env marker is written only when enabled. A new gated real Firecracker smoke test in `crates/core/src/execution/firecracker.rs` should stage a fake guest `codex` that runs `docker info`, writes a success marker into the workspace, and asserts that the daemon was actually available in the microVM.

Finally, update `config.example.toml`, `README.md`, and `guest/README.md` so operators know three separate Docker-related knobs now exist: Docker CLI in rootfs by default, explicit Docker registry auth, and optional guest Docker daemon startup.

## Concrete Steps

From repository root `/home/antonguzun/Work/personal/openoman`:

1. Edit `crates/core/src/execution/backend.rs` to add `docker_daemon: bool` to `FirecrackerBackendConfig`.
2. Edit `crates/cli/src/config.rs` to parse `[sandbox.firecracker].docker_daemon`, default it to `false`, and add config tests.
3. Edit `crates/core/src/execution/firecracker/runtime.rs` and `crates/core/src/execution/firecracker.rs` to export and test the guest env flag.
4. Edit `guest/openoman-init.sh` to mount `/run`, start `dockerd` when enabled, wait for readiness, and log failures.
5. Update docs in `config.example.toml`, `README.md`, and `guest/README.md`.
6. Add a real Firecracker smoke test in `crates/core/src/execution/firecracker.rs` that validates `docker info` inside the guest when `docker_daemon` is enabled.
7. Rebuild the rootfs with:

    ./guest/build-rootfs.sh ./guest/out

8. Run focused validation:

    cargo fmt --all
    cargo test -p openoman-cli config::tests
    cargo test -p openoman-core execution::firecracker::tests

9. Run the real Firecracker smoke test with the rebuilt assets:

    env PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" \
    CARGO_HOME=/tmp/openoman-cargo-home \
    OPENOMAN_FIRECRACKER_E2E=1 \
    OPENOMAN_FIRECRACKER_BIN="$HOME/.local/bin/firecracker" \
    OPENOMAN_FIRECRACKER_KERNEL="/home/antonguzun/Work/personal/openoman/guest/out/vmlinux" \
    OPENOMAN_FIRECRACKER_ROOTFS="/home/antonguzun/Work/personal/openoman/guest/out/rootfs.ext4" \
    cargo test -p openoman-core guest_docker_daemon_smoke_boots_real_firecracker_when_opted_in -- --nocapture

## Validation and Acceptance

Acceptance is met when all of the following are true:

- `AppConfig::load` reads `sandbox.firecracker.docker_daemon = true` and leaves it `false` when omitted.
- `render_agent_env` includes a guest-visible daemon-startup flag only when the operator enabled the feature.
- A Firecracker guest with the feature disabled behaves exactly as before and does not try to start Docker daemon.
- A Firecracker guest with the feature enabled writes `dockerd.log`, waits for readiness, and either succeeds with `docker info` or fails fast with a readable daemon log tail.
- The new real Firecracker smoke test passes against the rebuilt guest assets and proves `docker info` works inside the VM.

## Idempotence and Recovery

The config change is additive and safe to repeat. Rebuilding `guest/out/rootfs.ext4` remains idempotent because `guest/build-rootfs.sh` recreates the image from scratch each time.

If guest Docker daemon startup proves unstable in a deployment, removing or setting `sandbox.firecracker.docker_daemon = false` disables the new behavior without needing any cleanup in the workspace or runtime directories. If the real Firecracker smoke test fails after the feature is enabled, inspect `dockerd.log` in the collected output first; that log is the primary recovery artifact for kernel, cgroup, or storage-driver issues.

## Artifacts and Notes

Important evidence to capture while implementing:

- the exact `render_agent_env` line that carries the daemon flag
- the guest log line that announces Docker daemon startup
- the readiness success path from `docker info`
- the fallback or failure log tail from `dockerd.log` if startup fails

Representative success snippets should look like:

    OPENOMAN_DOCKER_DAEMON_ENABLED='1'
    starting guest docker daemon
    guest docker daemon is ready

and in the real smoke test:

    docker info
    Server Version: ...

## Interfaces and Dependencies

At completion, the following interfaces must exist or be updated:

- In `crates/core/src/execution/backend.rs`:

    pub struct FirecrackerBackendConfig {
        ...
        pub docker_daemon: bool,
        pub docker_auth_config: Option<DockerAuthConfig>,
        ...
    }

- In `crates/cli/src/config.rs`, extend `FirecrackerConfig` with:

    docker_daemon: Option<bool>

- In `crates/core/src/execution/firecracker/runtime.rs`, `render_agent_env(...)` must be able to emit:

    OPENOMAN_DOCKER_DAEMON_ENABLED='1'

- In `guest/openoman-init.sh`, define the guest boot behavior for:

    OPENOMAN_DOCKER_DAEMON_ENABLED

  so that it starts `dockerd`, waits for readiness, and logs failures to `/mnt/runtime/openoman-output/dockerd.log`.

Revision note (2026-03-16): Initial ExecPlan created after confirming the current blocker is missing guest daemon startup rather than missing Docker CLI or registry auth.
Revision note (2026-03-16): Completed implementation and validation, including a follow-up fix to switch the guest to legacy `iptables` so `dockerd` networking initializes successfully with the current Firecracker kernel config.
