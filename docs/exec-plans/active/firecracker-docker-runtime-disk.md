# Move Guest Docker Storage to the Firecracker Runtime Disk

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

`docs/PLANS.md` is checked into this repository and this document must be maintained in accordance with it.

## Purpose / Big Picture

After this change, Docker pulls and image builds inside a Firecracker guest will consume space from the per-attempt runtime disk (`/dev/vdb`, mounted at `/mnt/runtime`) instead of filling the guest root filesystem (`/dev/root`). Operators will also gain an explicit Firecracker config knob for the runtime disk size, so a Docker-heavy job can ask for a larger `/dev/vdb` without rebuilding the rootfs image.

The visible proof is that a Firecracker run with `sandbox.firecracker.docker_daemon = true` can report Docker's root directory as `/mnt/runtime/docker`, and a larger configured runtime disk results in a larger `runtime.ext4` image for that attempt.

## Progress

- [x] (2026-03-16 17:10Z) Confirmed the current failure mode from `job-1773671883831`: `dockerd` is healthy, but Docker stores data under `/var/lib/docker` on `/dev/root`, so image pulls fail with `no space left on device`.
- [x] (2026-03-16 17:13Z) Confirmed the current code paths: `guest/openoman-init.sh` starts `dockerd` without `--data-root`, `crates/core/src/execution/firecracker/runtime.rs` builds `/dev/vdb`, and `crates/cli/src/config.rs` does not expose any runtime disk size knob.
- [x] (2026-03-16 17:25Z) Added `sandbox.firecracker.runtime_disk_mb` to config loading, propagated it through `FirecrackerBackendConfig`, and aligned Firecracker disk quota with the explicit runtime disk size when configured.
- [x] (2026-03-16 17:28Z) Updated guest init so `dockerd` uses `--data-root=/mnt/runtime/docker`, moving Docker image and layer storage onto the per-attempt runtime disk.
- [x] (2026-03-16 17:32Z) Changed runtime image staging so an explicit runtime disk size controls the actual `runtime.ext4` file size, while the old dynamic sizing behavior remains the default when the knob is omitted.
- [x] (2026-03-16 17:45Z) Updated tests and docs, rebuilt the rootfs, and passed the real Firecracker Docker smoke test with the new Docker root directory under `/mnt/runtime/docker`.

## Surprises & Discoveries

- Observation: the current operator-visible failure is not caused by missing Docker auth or a broken daemon.
  Evidence: `workspaces/sandboxes/jobs/job-1773671883831/attempt-1/logs.txt` shows `guest docker daemon is ready` before `make test-local-up` fails on `failed to register layer ... no space left on device`.

- Observation: the runtime disk already exists and is mounted in the guest, but Docker is not using it for image storage.
  Evidence: `guest/openoman-init.sh` mounts `/dev/vdb` at `/mnt/runtime`, while the `dockerd` command currently omits `--data-root` and therefore keeps the Docker default `/var/lib/docker`.

- Observation: the current runtime image size logic treats `disk_quota_bytes` as a cap, not as the requested ext4 size.
  Evidence: `crates/core/src/execution/firecracker/runtime.rs::compute_image_size_bytes` returns `source_size + overhead` unless that exceeds the quota, which is why `/dev/vdb` stayed small in the failing job even though the internal quota default is larger.

- Observation: an explicit runtime disk size and a quota cap need separate validation paths if error messages should stay meaningful.
  Evidence: the first version of the focused test hit the generic quota error before the explicit-size branch could report that the configured runtime disk itself was too small, which required splitting “minimum required size” from “quota validation”.

## Decision Log

- Decision: expose explicit runtime disk sizing as `sandbox.firecracker.runtime_disk_mb`.
  Rationale: the problem is specific to the Firecracker backend and specifically to `/dev/vdb`, so a Firecracker-scoped setting is clearer than a generic top-level sandbox disk setting that could be misread as rootfs size.
  Date/Author: 2026-03-16 / Codex

- Decision: move Docker's data root to `/mnt/runtime/docker` instead of growing the rootfs image by default.
  Rationale: the runtime disk is per-attempt and disposable, which matches Docker layer and image storage for sandboxed jobs. This avoids bloating the shared rootfs image with transient job data.
  Date/Author: 2026-03-16 / Codex

- Decision: keep the current dynamic runtime image sizing as the default when `runtime_disk_mb` is omitted, but honor the explicit size exactly when it is configured.
  Rationale: this preserves the existing lightweight behavior for non-Docker jobs while giving operators a deterministic way to request larger runtime storage for Docker-heavy workloads.
  Date/Author: 2026-03-16 / Codex

- Decision: keep the runtime disk knob inside `[sandbox.firecracker]` instead of the top-level `[sandbox]` section.
  Rationale: the setting changes the Firecracker per-attempt ext4 disk (`/dev/vdb`), not the shared rootfs image or a backend-agnostic sandbox contract. Scoping the field to Firecracker avoids ambiguity.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The implementation reached the intended user-visible goal. Operators can now set `sandbox.firecracker.runtime_disk_mb` to request a larger per-attempt runtime disk, and guest `dockerd` stores images, layers, and build cache under `/mnt/runtime/docker` instead of `/var/lib/docker` on `/dev/root`.

The change landed in three connected parts. The config layer gained the explicit runtime disk knob. The Firecracker runtime builder now honors that explicit size as the actual `runtime.ext4` size when configured, while preserving the old dynamic staging behavior when the field is omitted. The guest init path now starts `dockerd` with `--data-root=/mnt/runtime/docker`, so Docker-heavy jobs actually consume the larger runtime disk when operators provide one.

Validation succeeded with:

- `cargo fmt --all`
- `cargo test -p openoman-cli config::tests`
- `cargo test -p openoman-core execution::firecracker::tests`
- `./guest/build-rootfs.sh ./guest/out`
- `env PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" CARGO_HOME=/tmp/openoman-cargo-home OPENOMAN_FIRECRACKER_E2E=1 OPENOMAN_FIRECRACKER_BIN="$HOME/.local/bin/firecracker" OPENOMAN_FIRECRACKER_KERNEL="/home/antonguzun/Work/personal/openoman/guest/out/vmlinux" OPENOMAN_FIRECRACKER_ROOTFS="/home/antonguzun/Work/personal/openoman/guest/out/rootfs.ext4" cargo test -p openoman-core guest_docker_daemon_smoke_boots_real_firecracker_when_opted_in -- --nocapture`

That final real Firecracker smoke test passed, proving that guest Docker daemon startup still works and that Docker root data now lives under `/mnt/runtime/docker`.

## Context and Orientation

The Firecracker backend runtime config is parsed in `crates/cli/src/config.rs`. That file reads `config.toml` and converts user-facing TOML fields into `openoman_core::execution::ExecutionRuntimeConfig` and `FirecrackerBackendConfig`.

`crates/core/src/execution/backend.rs` defines `FirecrackerBackendConfig`, which is the in-memory structure used by the Firecracker runner. If a new Firecracker setting should exist at runtime, it must be added there and populated by the CLI config loader.

`crates/core/src/execution/firecracker/runtime.rs` creates the per-attempt ext4 runtime image. That image becomes `/dev/vdb` inside the guest and is mounted at `/mnt/runtime` by `guest/openoman-init.sh`. This is the correct location for temporary per-job Docker data because it is disposable and recreated for each attempt.

`guest/openoman-init.sh` is the guest init process. It is responsible for mounting `/dev/vdb`, sourcing `agent.env`, and starting `dockerd` when `sandbox.firecracker.docker_daemon = true` is enabled. Today it starts `dockerd` without a custom data root, which means Docker writes to `/var/lib/docker` on `/dev/root`.

The concrete failure that motivates this change is recorded in `workspaces/sandboxes/jobs/job-1773671883831/attempt-1/logs.txt`. In that job, `dockerd` started successfully, but `make test-local-up` failed while pulling `postgres:15` because `/dev/root` had only about 230 MiB free. Moving Docker storage to `/mnt/runtime` and allowing `/dev/vdb` to be sized explicitly addresses that exact failure mode.

## Plan of Work

First, extend the Firecracker TOML config in `crates/cli/src/config.rs` with `runtime_disk_mb`. Parse it as an optional positive integer, convert it to bytes safely, and thread it into `FirecrackerBackendConfig`. When the operator provides this field, also use it to override the Firecracker execution limit `disk_quota_bytes` so the runtime image builder and the configured size cannot disagree.

Second, update `crates/core/src/execution/backend.rs` so `FirecrackerBackendConfig` carries the optional runtime disk size in bytes. Then update all helper constructors and tests that build `FirecrackerBackendConfig` manually.

Third, change `crates/core/src/execution/firecracker/runtime.rs` so runtime image sizing has two modes. When `runtime_disk_mb` is absent, keep today's dynamic sizing logic based on staged content plus overhead. When `runtime_disk_mb` is present, validate that the explicit size is large enough for the staged content and then build `runtime.ext4` at exactly that size. This makes the operator-visible knob affect the actual `/dev/vdb` filesystem size, not only an internal cap.

Fourth, update `guest/openoman-init.sh` so Docker stores images, writable layers, and build state under `/mnt/runtime/docker`. The `dockerd` command should use `--data-root=/mnt/runtime/docker`, and the script should ensure that directory exists before startup. This change must happen for both the `overlay2` path and the `vfs` fallback because both invoke the same helper.

Fifth, update tests and docs. Config tests in `crates/cli/src/config.rs` should verify that `runtime_disk_mb` parses correctly and influences limits. Firecracker runtime tests should verify that an explicit runtime disk size creates a matching `runtime.ext4` file and that the explicit size is rejected when it is too small. The real Firecracker Docker smoke test should verify Docker's root directory points at `/mnt/runtime/docker`, proving that job-local Docker data no longer lands on `/dev/root`.

Finally, document the new operator behavior in `config.example.toml`, `README.md`, and `guest/README.md`, and record the final implementation and validation results in this ExecPlan.

## Concrete Steps

From repository root `/home/antonguzun/Work/personal/openoman`:

1. Edit `crates/cli/src/config.rs` to add `runtime_disk_mb` to `FirecrackerConfig`, parse it, validate that it is positive, convert it to bytes, and thread it into both `FirecrackerBackendConfig` and Firecracker execution limits.
2. Edit `crates/core/src/execution/backend.rs` and any manual struct literals in tests to add `runtime_disk_bytes: Option<u64>` (or an equivalent explicit field name) to `FirecrackerBackendConfig`.
3. Edit `crates/core/src/execution/firecracker/runtime.rs` so explicit runtime disk size controls the actual ext4 size when configured, while preserving the current dynamic behavior when it is omitted.
4. Edit `guest/openoman-init.sh` so `dockerd` uses `--data-root=/mnt/runtime/docker`.
5. Update focused tests in `crates/cli/src/config.rs` and `crates/core/src/execution/firecracker.rs`.
6. Update `config.example.toml`, `README.md`, and `guest/README.md`.
7. Rebuild the rootfs with:

    ./guest/build-rootfs.sh ./guest/out

8. Run focused validation:

    cargo fmt --all
    cargo test -p openoman-cli config::tests
    cargo test -p openoman-core execution::firecracker::tests

9. Run the real Firecracker Docker smoke test with:

    env PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" \
    CARGO_HOME=/tmp/openoman-cargo-home \
    OPENOMAN_FIRECRACKER_E2E=1 \
    OPENOMAN_FIRECRACKER_BIN="$HOME/.local/bin/firecracker" \
    OPENOMAN_FIRECRACKER_KERNEL="/home/antonguzun/Work/personal/openoman/guest/out/vmlinux" \
    OPENOMAN_FIRECRACKER_ROOTFS="/home/antonguzun/Work/personal/openoman/guest/out/rootfs.ext4" \
    cargo test -p openoman-core guest_docker_daemon_smoke_boots_real_firecracker_when_opted_in -- --nocapture

## Validation and Acceptance

Acceptance is met when all of the following are true:

- `AppConfig::load` accepts `sandbox.firecracker.runtime_disk_mb = <N>` and reflects that value in the resulting Firecracker runtime config.
- A focused Firecracker runtime test proves that an explicit runtime disk size produces a `runtime.ext4` image of the configured size.
- A focused Firecracker runtime test rejects an explicit runtime disk size that is smaller than the staged content requires.
- A real Firecracker guest with Docker enabled reports Docker's root directory under `/mnt/runtime/docker`.
- The rootfs is rebuilt successfully and the focused Rust test suites remain green.

## Idempotence and Recovery

The config change is additive. If `runtime_disk_mb` is omitted, Firecracker keeps the old dynamic runtime image sizing behavior. If the operator provides a value that is too small, startup should fail before the VM boots with a readable configuration error rather than producing a partially working run.

Moving Docker storage to `/mnt/runtime/docker` is also safe to repeat because the runtime disk is per-attempt and discarded after the run. If a regression appears, removing `runtime_disk_mb` restores the old dynamic disk sizing, and setting `sandbox.firecracker.docker_daemon = false` disables the Docker path entirely.

## Artifacts and Notes

Important evidence to capture while implementing:

- the exact TOML example for `sandbox.firecracker.runtime_disk_mb`
- the `dockerd` launch path that now includes `/mnt/runtime/docker`
- the runtime image size assertion from focused tests
- the real Firecracker smoke-test evidence that Docker root dir moved off `/var/lib/docker`

Representative success snippets should look like:

    runtime_disk_mb = 4096

    guest docker daemon is ready
    Docker Root Dir: /mnt/runtime/docker

## Interfaces and Dependencies

At completion, the following interfaces must exist or be updated:

- In `crates/cli/src/config.rs`, extend `FirecrackerConfig` with:

    runtime_disk_mb: Option<u64>

- In `crates/core/src/execution/backend.rs`, extend `FirecrackerBackendConfig` with:

    pub runtime_disk_bytes: Option<u64>

- In `guest/openoman-init.sh`, the `dockerd` launch command must include:

    --data-root=/mnt/runtime/docker

- In `crates/core/src/execution/firecracker/runtime.rs`, runtime image staging must choose between:

    1. dynamic size based on staged content plus overhead, when `runtime_disk_mb` is omitted
    2. explicit configured ext4 size, when `runtime_disk_mb` is present

Revision note (2026-03-16): Initial ExecPlan created after confirming that Docker storage still lands on `/dev/root` and that the current runtime disk quota does not control the actual `/dev/vdb` filesystem size.
Revision note (2026-03-16): Completed implementation and validation, including a helper split between minimum runtime image size and quota validation so explicit runtime disk errors are reported clearly.
