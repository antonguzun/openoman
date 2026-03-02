# Firecracker backend modes with startup validation

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `/docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, `openoman` no longer pretends to use a microVM. The runtime selects a virtualization backend from config, validates that backend when the Rust application starts, and runs sandbox attempts through a real Firecracker-oriented backend abstraction. Users can choose between `firecracker` direct mode and a future `firecracker` plus `jailer` mode. Direct mode works in this iteration; Jailer mode is surfaced in config but fails early with a clear message that it needs a more prepared host environment.

## Progress

- [x] (2026-03-02 12:40Z) Reviewed the current dummy runner, CLI config loading, existing ExecPlans, and Firecracker host availability on this machine.
- [x] (2026-03-02 12:54Z) Confirmed `firecracker` and `jailer` are installed, `/dev/kvm` exists, and plain-user `jailer` fails due to mount namespace privilege requirements.
- [x] (2026-03-02 13:14Z) Refactored the core sandbox module into backend-neutral files under `/crates/core/src/sandbox/` and added Firecracker backend/mode/config types.
- [x] (2026-03-02 13:21Z) Implemented startup dependency validation, a direct-mode Firecracker runner, and explicit `jailer`-mode rejection.
- [x] (2026-03-02 13:27Z) Wired CLI config parsing and run flow to the backend factory while preserving the existing artifact pipeline.
- [x] (2026-03-02 13:34Z) Replaced sandbox tests and CLI end-to-end tests with fake Firecracker fixtures covering direct mode and Jailer-mode startup failure.
- [x] (2026-03-02 13:36Z) Added guest asset documentation plus `guest/build-rootfs.sh` and `guest/openoman-init.sh`, then ran `cargo fmt --all` and `cargo test --all-targets`.
- [x] (2026-03-02 14:18Z) Added kernel-download and asset-pair build scripts, produced a real `guest/out/{vmlinux,rootfs.ext4}` pair on this machine, and documented the smoke-test workflow.
- [x] (2026-03-02 14:26Z) Added an opt-in real Firecracker smoke test, validated it outside the sandbox against the built assets, and hardened the direct runner with a writable per-run rootfs copy plus a guest completion marker.

## Surprises & Discoveries

- Observation: `jailer` cannot run as the current unprivileged user on this host.
  Evidence: `jailer --id plan-probe ...` fails with `Failed to unshare into new mount namespace: Operation not permitted`.
- Observation: the local Firecracker binary supports `--config-file`, which is enough for an MVP direct-mode runner without implementing a custom HTTP control client first.
  Evidence: `firecracker --help` documents `--config-file <config-file>` and `--no-api`.
- Observation: the host already has `mkfs.ext4`, `debugfs`, `dumpe2fs`, and `e2fsck`.
  Evidence: `command -v` resolves each tool under `/usr/sbin`.
- Observation: the practical direct-mode data path can stay unprivileged by using `mkfs.ext4 -d` for staging and `debugfs rdump/dump` for collection.
  Evidence: the fake Firecracker lifecycle tests and CLI end-to-end tests pass with this approach.
- Observation: a shared read-only guest rootfs is incompatible with bind-mounting package directories into arbitrary guest paths.
  Evidence: the first real Firecracker smoke run booted, ran the guest, and then crashed because `/opt/openoman/...` targets lived on a read-only root filesystem.
- Observation: a guest-side completion marker is more reliable than waiting for the VM process to power itself off cleanly.
  Evidence: the real Firecracker smoke run completed guest work and wrote artifacts, but the VM stayed alive until timeout until the host started polling `openoman-output/exit-code.txt`.

## Decision Log

- Decision: implement `firecracker` direct mode now and represent `firecracker` plus `jailer` as a config-visible but unavailable mode.
  Rationale: this preserves the future interface while avoiding a privileged-helper or privileged-main-process requirement in this iteration.
  Date/Author: 2026-03-02 / Codex
- Decision: stage sandbox runtime input into an ext4 image and read sandbox output back out with `debugfs`.
  Rationale: this avoids requiring host root mounts or a host-side vsock protocol for the MVP implementation.
  Date/Author: 2026-03-02 / Codex
- Decision: copy configured host-user package directories into the per-run runtime image rather than live-mounting them.
  Rationale: copying is simpler, keeps direct mode unprivileged, and still makes explicitly allowlisted user tools available inside the guest.
  Date/Author: 2026-03-02 / Codex
- Decision: copy the configured rootfs image into the per-run directory and boot it read-write.
  Rationale: this keeps the shared rootfs asset immutable on the host while allowing guest setup steps such as package bind targets to live on an ephemeral writable filesystem.
  Date/Author: 2026-03-02 / Codex
- Decision: treat guest completion as a protocol signal stored in the runtime image instead of requiring Firecracker to exit on its own.
  Rationale: direct mode currently lacks a reliable guest-triggered shutdown path in this environment, but the host can safely kill Firecracker once the guest has synced an exit-code marker.
  Date/Author: 2026-03-02 / Codex

## Outcomes & Retrospective

The implementation landed with a real Firecracker-shaped backend abstraction and a working direct-mode runner contract. `openoman` now validates the configured sandbox backend on startup, the CLI run path constructs its sandbox runner through the backend factory, and both core and CLI tests exercise the new direct-mode flow with a fake Firecracker binary that mutates the staged ext4 runtime image.

The guest asset workflow is now concrete rather than only documented. `guest/build-assets.sh` downloads a versioned Firecracker `firecracker-ci` kernel, builds a rootfs image, and produces a usable pair under `guest/out/`. The new opt-in smoke test booted a real Firecracker VM on this host, staged a shell-based fake `codex` through `user_package_dirs`, verified guest logs/report generation, and proved the host can collect a modified workspace back out of the VM path.

The most important gap that remains is network plumbing for a real user-mode guest workload. Direct mode currently focuses on the microVM lifecycle, workspace/package staging, and artifact extraction. It does not yet create a guest network device, so fully remote agent execution still depends on future networking work. Jailer mode remains config-visible and intentionally unavailable until the host deployment model is clarified.

## Context and Orientation

The existing sandbox code lives in `/crates/core/src/sandbox.rs`. It exposes `SandboxRunner`, `AttemptSpec`, and related types, but the concrete `FirecrackerRunner` only runs a host shell script. The CLI at `/crates/cli/src/main.rs` constructs that fake runner directly in the `Run` command path.

The configuration files `/config.toml` and `/config.example.toml` currently only provide a sandbox runtime directory. There is no backend-selection setting, no Firecracker asset configuration, and no startup validation for runtime dependencies.

The runtime artifact contract is already valuable and must not change. `openoman run <job_id>` prepares trusted and sandbox workspaces, runs a sandbox attempt, stores `workspace.sandbox_result`, writes a canonical patch on the trusted host, and persists `sandbox.patch`, `sandbox.report`, and `sandbox.logs`.

## Plan of Work

First, split the sandbox module into backend-neutral pieces so the CLI can depend on a stable interface instead of a concrete runner type. Add `SandboxBackendKind`, `FirecrackerMode`, `SandboxRuntimeConfig`, `FirecrackerBackendConfig`, and `UserPackageDir` in core. Add a `SandboxBackend` trait with `check_runtime_dependencies()` and `create_runner()`.

Next, implement a Firecracker backend in `/crates/core/src/sandbox/firecracker.rs`. Direct mode will validate `firecracker`, `/dev/kvm`, kernel/rootfs paths, and ext4 helper tools. Jailer mode will validate `jailer` exists and then return a not-implemented error that explains why the host still needs more preparation. The direct runner will stage the prepared workspace plus copied package directories into a runtime directory, build an ext4 image with `mkfs.ext4 -d`, start Firecracker with `--config-file` and `--no-api`, wait with timeout handling, and then dump the modified workspace plus report/log files from the ext4 image into the stable output directory with `debugfs`.

Then, update `/crates/cli/src/main.rs` so config parsing produces `SandboxRuntimeConfig`, startup always validates the selected backend, and the `Run` command creates the runner through the backend factory. Preserve the rest of the artifact pipeline and host-side patch generation logic.

Finally, rewrite tests to use a fake Firecracker executable that mutates the staged ext4 image instead of relying on the old shell-script runner. Add config parsing tests, dependency validation tests, direct-mode lifecycle tests, CLI end-to-end tests for the direct backend, and CLI failure coverage for Jailer mode.

## Concrete Steps

From the repository root:

    cargo fmt --all
    cargo test --all-targets

Opt-in smoke validation after implementation:

    OPENOMAN_FIRECRACKER_E2E=1 cargo test -p openoman-core firecracker -- --nocapture

Expected behavior after the feature lands:

    openoman --config ./config.toml status <job_id>

will fail immediately if the configured backend dependencies are missing, and:

    openoman --config ./config.toml run <job_id>

will use the selected backend rather than the old dummy host subprocess path.

## Validation and Acceptance

Acceptance is met when:

1. Config supports Firecracker backend selection and `direct` versus `jailer` mode.
2. Startup dependency validation fails fast with clear messages when the selected runtime is unavailable.
3. Direct mode uses the backend abstraction and produces `workspace.sandbox_result`, `sandbox.patch`, `sandbox.report`, and `sandbox.logs`.
4. Explicitly configured user package directories are copied into the runtime image and exposed to the guest contract.
5. Jailer mode is rejected early with a clear error explaining that it needs additional environment preparation.

## Idempotence and Recovery

The runner uses per-attempt runtime directories under the configured sandbox runtime root and stable output directories under `jobs/<job_id>/attempt-<attempt_id>`. Re-running tests or repeated failed attempts can safely recreate these directories because each run removes any pre-existing per-attempt staging/output directory before rebuilding it.

## Artifacts and Notes

The stable artifact names remain:

    workspace.trusted_clone
    workspace.sandbox_input
    workspace.sandbox_result
    sandbox.patch
    sandbox.report
    sandbox.logs

The new direct-mode runtime will additionally write Firecracker process logs into the per-run ephemeral directory to make VM startup failures diagnosable even when the guest does not produce report/log files.

## Interfaces and Dependencies

The final core sandbox interface should expose:

    pub enum SandboxBackendKind {
        Firecracker,
    }

    pub enum FirecrackerMode {
        Direct,
        Jailer,
    }

    pub trait SandboxBackend {
        fn kind(&self) -> SandboxBackendKind;
        fn check_runtime_dependencies(&self) -> Result<(), SandboxError>;
        fn create_runner(&self) -> Result<Box<dyn SandboxRunner>, SandboxError>;
    }

    pub fn build_sandbox_backend(
        config: SandboxRuntimeConfig,
    ) -> Result<Box<dyn SandboxBackend>, SandboxError>;

The Firecracker direct-mode implementation depends on the host commands `tar`, `mkfs.ext4`, `debugfs`, and `e2fsck`, plus the configured Firecracker binary and guest assets.

Revision note (2026-03-02): Created this ExecPlan at implementation start because the change crosses core runtime abstractions, CLI configuration, host dependency validation, and end-to-end sandbox behavior.

Revision note (2026-03-02): Updated the living sections after implementation to record completed milestones, the ext4 staging/collection discovery, and the remaining direct-mode networking gap.

Revision note (2026-03-02): Extended the plan after follow-up work to cover real guest asset generation, the opt-in real Firecracker smoke test, the writable per-run rootfs copy, and the guest completion-marker protocol.
