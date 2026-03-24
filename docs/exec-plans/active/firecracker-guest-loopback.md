# Firecracker guest loopback for Docker published ports

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `docs/PLANS.md`.

## Purpose / Big Picture

After this change, a job that starts Docker inside the Firecracker guest can publish container ports to `127.0.0.1` inside the guest without failing with `bind: cannot assign requested address`. The user-visible effect is that Docker-based test stacks which expose services on loopback can start normally inside sandboxed jobs instead of failing before tests even run.

## Progress

- [x] (2026-03-17 11:47Z) Investigated the failed `job-1773679067303` artifacts and confirmed the representative runtime error was `Cannot start service redis: ... 127.0.0.1:6379: bind: cannot assign requested address`.
- [x] (2026-03-17 11:49Z) Traced the failure to the guest bootstrap script: `guest/openoman-init.sh` configured `eth0` when needed but never brought up the `lo` interface.
- [ ] Implement the guest loopback initialization change and keep the rest of guest bootstrap behavior unchanged.
- [ ] Add regression coverage for the loopback bootstrap contract and, where practical, extend the opt-in real Firecracker Docker smoke test to exercise localhost port publishing.
- [ ] Run focused validation and record the observed outcome here.

## Surprises & Discoveries

- Observation: The failure was not specific to any repository logic. It happened before application tests ran, at the Docker port-publish layer inside the guest.
  Evidence: The job logs recorded `Cannot start service redis: ... 127.0.0.1:6379: bind: cannot assign requested address` while `docker compose ...` was still starting infrastructure containers.

- Observation: The current guest init script never configures loopback.
  Evidence: `guest/openoman-init.sh` mounts filesystems, optionally configures `eth0`, and starts `dockerd`, but contains no `ip link set lo up` or `ip addr add 127.0.0.1/8 dev lo`.

## Decision Log

- Decision: Fix the guest runtime rather than introducing repository-specific workarounds.
  Rationale: The user clarified that `openoman` must stay repository-agnostic. A missing loopback interface is guest infrastructure state, not application state.
  Date/Author: 2026-03-17 / Codex

- Decision: Keep the fix minimal by initializing `lo` during guest bootstrap before the Docker daemon is used.
  Rationale: Docker localhost publishes rely on the standard loopback address being present. Restoring that baseline avoids coupling the runtime to any particular Docker Compose file.
  Date/Author: 2026-03-17 / Codex

## Outcomes & Retrospective

Implementation is in progress. The expected outcome is that Firecracker guest jobs with `docker_daemon = true` can publish to `127.0.0.1` and continue past infrastructure startup.

## Context and Orientation

The Firecracker guest contract is implemented by `guest/openoman-init.sh`. That script becomes `/sbin/openoman-init` inside the guest root filesystem and is the first process that runs in the VM. It mounts the runtime disk, loads `agent.env`, optionally configures guest networking, optionally starts `dockerd`, and then runs the agent.

The host-side Firecracker execution path lives in `crates/core/src/execution/firecracker.rs` and `crates/core/src/execution/firecracker/runtime.rs`. The existing opt-in real Firecracker smoke tests in `crates/core/src/execution/firecracker.rs` already prove that the guest boots and that `dockerd` can start. They do not yet prove that Docker can publish to guest loopback.

The failure being fixed here comes from jobs that run Docker Compose inside the guest and publish service ports to `127.0.0.1`. In Linux, those binds require the loopback interface `lo` to be up with `127.0.0.1/8` assigned. The current guest script does not do that, so Docker reports that the address cannot be assigned.

## Plan of Work

First, update `guest/openoman-init.sh` so it initializes loopback as soon as the `ip` command is known to exist. The script should bring `lo` up and ensure that `127.0.0.1/8` is present. The commands must be safe to run repeatedly because the guest bootstrap may be retried during development.

Second, add regression coverage in `crates/core/src/execution/firecracker.rs`. The low-cost regression check should assert that the checked-in guest init script still contains loopback setup commands. If the existing opt-in real Firecracker Docker smoke test can be extended cheaply, make it publish an HTTP port on `127.0.0.1` inside the guest and verify that the guest can reach it.

Third, update any nearby guest-runtime documentation in `guest/README.md` if the runtime contract description should mention loopback initialization explicitly.

## Concrete Steps

From the repository root:

1. Edit `guest/openoman-init.sh` to initialize `lo` before Docker or application commands depend on localhost.
2. Edit `crates/core/src/execution/firecracker.rs` to add regression coverage for the loopback bootstrap contract and, if practical, strengthen the opt-in Docker smoke test.
3. Update `guest/README.md` if the contract description needs to mention loopback initialization.
4. Run focused tests from the repository root:

       cargo test -p openoman-core firecracker

   If the local Firecracker assets are available, also run:

       OPENOMAN_FIRECRACKER_E2E=1 cargo test -p openoman-core guest_docker_daemon_smoke_boots_real_firecracker_when_opted_in -- --nocapture

## Validation and Acceptance

Acceptance is:

- the guest init script brings up loopback before starting `dockerd` or the agent;
- the focused Rust tests pass;
- if the opt-in real Firecracker smoke test is run, it proves that the guest Docker daemon can still start and, ideally, that a container published on `127.0.0.1` is reachable from inside the guest.

The original failure should no longer be reproducible for a Docker Compose stack whose only blocker was missing guest loopback.

## Idempotence and Recovery

The loopback setup commands must be safe to run on every guest boot. If `127.0.0.1/8` is already configured, the script should tolerate that and continue. If the real Firecracker smoke test is unavailable on the current machine, the focused Rust tests still provide regression coverage and the missing end-to-end validation must be noted.

## Artifacts and Notes

Representative failure excerpt from the investigated job:

    Cannot start service redis: ... listen tcp4 127.0.0.1:6379: bind: cannot assign requested address

Key implementation location:

    guest/openoman-init.sh

## Interfaces and Dependencies

No public Rust interface changes are required. The implementation relies on the existing guest dependency on `iproute2`, which already provides the `ip` command inside the guest image. The relevant checked-in files after completion should still be:

- `guest/openoman-init.sh` as the guest bootstrap contract,
- `crates/core/src/execution/firecracker.rs` for runtime regression coverage,
- optionally `guest/README.md` for contract documentation.

Change note: Created on 2026-03-17 to track the localhost Docker publish failure observed in `job-1773679067303` and to keep the fix repository-agnostic.
