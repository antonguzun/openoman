# Firecracker Virtio RNG Support

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, Firecracker direct-mode guests started by `openoman run <job_id>` have a virtual entropy device. In plain terms, the guest kernel can read random bytes from the host instead of stalling while programs wait for entropy during startup. The immediate user-visible outcome is that guest-side `node` and the Codex CLI can progress past the `getrandom` stall that currently prevents any patch generation. You can see the improvement by rerunning the existing `kickfoss` reproduction job flow and observing that guest logs advance past `running node codex entrypoint --version`.

## Progress

- [x] (2026-03-02 13:52Z) Confirmed with in-guest `strace` that the current Codex startup stalls in `getrandom`, leaving `sandbox.patch` empty because no workspace files change.
- [x] (2026-03-02 13:57Z) Reviewed the Firecracker backend JSON writer and the official Firecracker schema to identify the correct top-level `entropy` config shape.
- [x] (2026-03-02 14:00Z) Implemented the Firecracker JSON change so direct mode always enables a virtio RNG device.
- [x] (2026-03-02 14:00Z) Added unit coverage that inspects the generated Firecracker config and proves the `entropy` object is present.
- [x] (2026-03-02 14:03Z) Validated the change against the real `kickfoss` reproduction flow and captured the new behavior with preserved transient Firecracker config and boot logs.
- [x] (2026-03-02 14:04Z) Updated guest asset documentation to explain that the default downloaded `hello-vmlinux.bin` kernel is still too minimal to consume the entropy device.
- [x] (2026-03-02 14:18Z) Verified Firecracker's official `firecracker-ci/v1.12/{x86_64,aarch64}/vmlinux-6.1.128` artifacts and their published configs both include `CONFIG_HW_RANDOM_VIRTIO=y`.
- [x] (2026-03-02 14:19Z) Switched the guest kernel downloader to the official `firecracker-ci` kernel by default, persisted the matching `.config` beside the image, and made the download fail fast if virtio-rng support is missing.
- [x] (2026-03-02 14:20Z) Rebuilt `guest/out/vmlinux` from the new default and reran the `kickfoss` reproduction flow as `job-1772461099382`.
- [x] (2026-03-02 14:20Z) Confirmed the guest now progresses past the old `getrandom` stall: `node "$codex_target" --version` and `codex --version` both return `codex-cli 0.106.0`, and the run reaches live `codex exec`.
- [x] (2026-03-02 14:20Z) Captured the new remaining blocker: `codex exec` times out after a reconnect error while sending a request to `https://api.openai.com/v1/responses`, which is consistent with the known lack of guest networking.

## Surprises & Discoveries

- Observation: the guest rootfs, Node runtime, and Codex entrypoint are all present and executable, but the direct Node probe still hangs before `codex --version` returns.
  Evidence: stored `sandbox.logs` for `job-1772459637766` show `running timed strace on node codex entrypoint --version`, followed by a forced timeout and a trace tail ending in `getrandom`.
- Observation: the current Firecracker config builder does not include any RNG or entropy device, only boot source, drives, and machine config.
  Evidence: `crates/core/src/sandbox/firecracker.rs` builds `FirecrackerConfigFile` with only `boot-source`, `drives`, and `machine-config`.
- Observation: Firecracker’s current official schema models entropy as a top-level `entropy` object, not as part of `machine-config` or `boot-source`.
  Evidence: the official `firecracker.yaml` in the Firecracker repository defines top-level `/entropy` and `definitions.EntropyDevice`.
- Observation: the live generated Firecracker JSON now contains `"entropy": {}` and the guest boots with a third `virtio-mmio` device, so the backend wiring is active.
  Evidence: preserved `/tmp/job-1772460204551-capture/firecracker-config.json` shows `"entropy": {}`, and the matching serial log shows `virtio-mmio.2` registered at `0xc0003000-0xc0003fff`.
- Observation: even with the entropy device present, the current downloaded `guest/out/vmlinux` kernel still blocks the guest Codex startup in `getrandom`.
  Evidence: preserved `sandbox.logs` for `job-1772460204551` still end in a timed `node` probe with a trace tail terminating at `getrandom`.
- Observation: the locally downloaded sample kernel appears not to include the virtio-rng driver, even though Firecracker’s current CI kernel configs do enable it.
  Evidence: `strings guest/out/vmlinux | rg 'virtio_rng|HW_RANDOM_VIRTIO'` returned no matches, while the official `resources/guest_configs/microvm-kernel-ci-x86_64-{5.10,6.1}.config` files both contain `CONFIG_HW_RANDOM_VIRTIO=y`.
- Observation: Firecracker publishes versioned guest kernels and sidecar `.config` files on the same public `spec.ccfc.min` bucket, so the repository can pin a real upstream-tested kernel rather than a custom build.
  Evidence: `curl -I https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/x86_64/vmlinux-6.1.128` returned `200 OK`, and `curl .../vmlinux-6.1.128.config | rg '^CONFIG_HW_RANDOM_VIRTIO='` returned `CONFIG_HW_RANDOM_VIRTIO=y`.
- Observation: once the guest boots the `firecracker-ci` kernel, the previous entropy stall disappears completely.
  Evidence: the `serial.log` for `job-1772461099382` shows `random: crng init done`, and the corresponding `sandbox logs` show both `running timed strace on node codex entrypoint --version` and `running timed codex --version` ending with `codex-cli 0.106.0` and exit status `0`.
- Observation: the next end-to-end blocker is network reachability from inside the guest, not entropy.
  Evidence: the same `sandbox logs` for `job-1772461099382` continue into `running codex exec in /mnt/runtime/workspace` and then fail with `Reconnecting... 1/5 (stream disconnected before completion: error sending request for url (https://api.openai.com/v1/responses))` before the 60-second sandbox timeout expires.

## Decision Log

- Decision: enable the entropy device unconditionally for direct Firecracker mode instead of introducing a user-facing config toggle first.
  Rationale: the current direct runner has no meaningful case where guest entropy is harmful, and the immediate blocker is a basic guest startup failure rather than an optional optimization.
  Date/Author: 2026-03-02 / Codex
- Decision: use the minimal Firecracker entropy config, `entropy: {}`, rather than exposing rate limiting in this change.
  Rationale: the goal is to restore correct guest startup behavior with the smallest surface area; rate limiting can be added later if needed.
  Date/Author: 2026-03-02 / Codex
- Decision: keep the backend entropy device change even though the current default downloaded sample kernel cannot consume it.
  Rationale: the Firecracker-side support is still correct and testable, and future guest kernels that include `virtio-rng` will benefit immediately. The remaining gap is now clearly isolated to guest asset choice rather than backend wiring.
  Date/Author: 2026-03-02 / Codex
- Decision: make the default guest kernel the official Firecracker `firecracker-ci/v1.12/.../vmlinux-6.1.128` artifact and download its sidecar `.config` into `guest/out/vmlinux.config`.
  Rationale: this keeps the asset workflow simple, uses an upstream-tested kernel, and gives the repository a machine-checkable proof that the downloaded kernel enables `CONFIG_HW_RANDOM_VIRTIO=y`.
  Date/Author: 2026-03-02 / Codex

## Outcomes & Retrospective

The direct Firecracker backend now emits a top-level `entropy` object in every generated config, and unit coverage proves that serialization. Real validation also confirmed that Firecracker accepts the config and the guest sees a third `virtio-mmio` device, so the backend work itself is complete.

The direct Firecracker backend now emits a top-level `entropy` object in every generated config, and the guest asset workflow now defaults to Firecracker's official `firecracker-ci` kernel that publishes `CONFIG_HW_RANDOM_VIRTIO=y` in its matching config. A fresh real run (`job-1772461099382`) proved the intended fix: the guest no longer hangs in `getrandom`, `node` and `codex --version` complete, and `codex exec` starts normally.

The remaining failure is now a different subsystem. `codex exec` reached the point of trying to call `https://api.openai.com/v1/responses`, then retried and hit the 60-second sandbox timeout. That matches the repository's existing limitation that direct Firecracker mode does not yet wire a guest network device, so the entropy objective is complete and the next practical track is guest networking.

## Context and Orientation

The Firecracker backend lives in `crates/core/src/sandbox/firecracker.rs`. That file stages the per-run runtime image, copies the configured root filesystem, generates the one-shot Firecracker JSON config, launches the Firecracker process, waits for completion or timeout, and collects output artifacts back to the host. In the current repository state it already emits a top-level `entropy` object, so every direct-mode VM gets a virtio-rng device from Firecracker.

The guest asset workflow lives under `guest/`. `guest/download-firecracker-kernel.sh` downloads the kernel image into `guest/out/vmlinux`. `guest/build-rootfs.sh` builds `guest/out/rootfs.ext4` with `/sbin/openoman-init` and the guest-side Node/Codex toolchain. `guest/build-assets.sh` is the wrapper that prepares both. The asset choice matters because Firecracker can expose a virtio-rng device while the guest kernel still ignores it if that driver was not compiled in.

The reproduction path for this plan is a real `openoman` run against `git@github.com:antonguzun/kickfoss.git` with the instruction `add 12345 into end of README`. Before the kernel switch, guest logs showed Node startup hanging in `getrandom`, so `codex exec` never started and `sandbox.patch` stayed empty. After the kernel switch validated here, the entropy problem is gone and the remaining blocker is guest network reachability for the Codex API call.

## Plan of Work

First, keep the existing Firecracker backend entropy support in place and change the guest kernel asset source instead. In `guest/download-firecracker-kernel.sh`, replace the old sample `hello-vmlinux.bin` URL with the official Firecracker `firecracker-ci` kernel URL for each supported architecture, derive the sidecar `.config` URL, and download both into `guest/out/`.

Second, make the kernel download self-validating. After downloading `guest/out/vmlinux.config`, inspect it for `CONFIG_HW_RANDOM_VIRTIO=y` and fail fast if that option is missing. This keeps the repository from silently falling back to another entropy-starved kernel in future rebuilds. Update `guest/README.md` so a novice understands the new default, the new `guest/out/vmlinux.config` artifact, and the override variables `OPENOMAN_FIRECRACKER_KERNEL_URL` and `OPENOMAN_FIRECRACKER_KERNEL_CONFIG_URL`.

Third, rebuild `guest/out/vmlinux` from the new default and rerun the `kickfoss` reproduction flow. The expected proof is no longer just “the VM has an entropy device”; it is that guest logs advance through `node --version`, `node .../codex.js --version`, `codex --version`, and into live `codex exec`. Record the next blocker if the run still does not produce a patch.

## Concrete Steps

From the repository root:

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin cargo test -p openoman-core firecracker -- --nocapture

Rebuild the default kernel asset and verify the pinned config:

    ./guest/download-firecracker-kernel.sh ./guest/out
    rg '^CONFIG_HW_RANDOM_VIRTIO=y$' ./guest/out/vmlinux.config

Rerun the reproduction:

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin ./target/debug/openoman submit --repo git@github.com:antonguzun/kickfoss.git --revision main --instruction 'add 12345 into end of README'
    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin ./target/debug/openoman run <job_id>
    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin ./target/debug/openoman logs <job_id>

Observed proof for the completed kernel switch:

    kernel image downloaded to ./guest/out/vmlinux
    kernel config downloaded to ./guest/out/vmlinux.config
    ...
    running timed codex --version
    codex-cli 0.106.0
    timed codex --version exit status: 0
    running codex exec in /mnt/runtime/workspace

## Validation and Acceptance

Acceptance is satisfied when all of the following are true:

1. A unit test proves the generated Firecracker JSON now contains the top-level `entropy` object.
2. `cargo test -p openoman-core firecracker -- --nocapture` passes locally.
3. `./guest/download-firecracker-kernel.sh ./guest/out` succeeds and writes both `guest/out/vmlinux` and `guest/out/vmlinux.config`.
4. The downloaded `guest/out/vmlinux.config` contains `CONFIG_HW_RANDOM_VIRTIO=y`.
5. A fresh real `openoman run` against the `kickfoss` reproduction job proves the guest progresses past the old `getrandom` stall and reaches live `codex exec`.
6. If the run still does not finish successfully, the plan must record the new blocker with concrete logs.

## Idempotence and Recovery

The Firecracker JSON change is additive and safe to rerun. Re-running `./guest/download-firecracker-kernel.sh ./guest/out` overwrites `guest/out/vmlinux` and `guest/out/vmlinux.config` with the currently pinned upstream artifacts. Rebuilding the guest rootfs with `./guest/build-rootfs.sh ./guest/out` remains idempotent because the script recreates `guest/out/rootfs.ext4`. If a validation job fails midway, rerun `submit` to get a fresh `job_id`; existing failed jobs remain useful for inspecting logs and artifacts.

## Artifacts and Notes

Current failure evidence before this change:

    sandbox logs:
    ...
    running timed strace on node codex entrypoint --version
    Killed
    timed node codex probe exit status: 137
    ...
    13:54:06.294382 getrandom

This is the concrete symptom the virtio RNG device is intended to remove. After implementation, the remaining evidence shows the backend side is fixed but the current default guest kernel still lacks the corresponding driver support.

Current successful proof after the kernel switch:

    sandbox logs:
    ...
    running timed strace on node codex entrypoint --version
    codex-cli 0.106.0
    timed node codex probe exit status: 0
    ...
    running timed codex --version
    codex-cli 0.106.0
    timed codex --version exit status: 0
    running codex exec in /mnt/runtime/workspace
    ...
    Reconnecting... 1/5 (stream disconnected before completion: error sending request for url (https://api.openai.com/v1/responses))

## Interfaces and Dependencies

In `crates/core/src/sandbox/firecracker.rs`, the generated config should end with a struct shape equivalent to:

    struct FirecrackerConfigFile {
        boot_source: BootSourceConfig,
        drives: Vec<DriveConfig>,
        machine_config: MachineConfig,
        entropy: EntropyDeviceConfig,
    }

    struct EntropyDeviceConfig {
        rate_limiter: Option<...>,
    }

For this change, `rate_limiter` may remain unset and serialize as an empty object.

Revision note (2026-03-02): Created this plan during implementation after confirming guest startup is blocked in `getrandom` and before modifying the Firecracker JSON writer.
Revision note (2026-03-02): Updated the living sections after implementation, unit validation, real-run capture, and the discovery that the default downloaded sample kernel still lacks virtio-rng support.
Revision note (2026-03-02): Revised the plan after verifying Firecracker's official `firecracker-ci` kernel artifacts and switching the guest asset downloader to pin a kernel/config pair that proves `CONFIG_HW_RANDOM_VIRTIO=y`.
Revision note (2026-03-02): Updated the plan after rebuilding the pinned kernel asset and validating with `job-1772461099382` that the `getrandom` stall is resolved and the next blocker is guest networking/API connectivity.
