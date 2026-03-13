# Firecracker guest networking through a host allowlisting proxy

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `docs/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, a real Firecracker sandbox can reach the Codex API without opening unrestricted guest internet access. `openoman run <job_id>` now has a concrete host-proxy networking mode that attaches a tap-backed NIC to the guest, assigns a static guest IPv4 address, points `HTTP_PROXY` and `HTTPS_PROXY` at a host-local HTTP CONNECT proxy, and enforces `agent.egress_allowed_domains` on the host. A user can observe this working by seeing guest logs mention `host-proxy` networking and by seeing host proxy decisions appended to `sandbox.logs`.

## Progress

- [x] (2026-03-02 16:52Z) Re-verified the existing Firecracker direct backend, the guest asset contract, and the missing-networking failure mode from the current repository state.
- [x] (2026-03-02 16:53Z) Confirmed the host constraint that matters most for implementation: the current user can run Firecracker and KVM, but cannot create tap devices or inspect firewall state without privilege escalation.
- [x] (2026-03-02 17:14Z) Added networking config types in the core sandbox contract and CLI config parsing for `[sandbox.firecracker.network]`.
- [x] (2026-03-02 17:24Z) Added hidden CLI helper subcommands for tap setup and teardown plus `sudo -v` priming in the normal `run` path.
- [x] (2026-03-02 17:36Z) Implemented deterministic per-run `/30` lease allocation, Firecracker NIC JSON emission, host proxy lifecycle management, and tap cleanup in the direct runner.
- [x] (2026-03-02 17:44Z) Updated the guest init contract to configure `eth0` statically and export the host proxy before Codex startup.
- [x] (2026-03-02 17:52Z) Added unit and CLI coverage for host-proxy config parsing, helper bypass behavior, deterministic lease allocation, NIC JSON rendering, proxy deny behavior, and guest env rendering.
- [ ] (remaining) Validate the full end-to-end `codex exec` path against the real OpenAI API from this host once `sudo` credentials are available for a real tap lifecycle run.

## Surprises & Discoveries

- Observation: the local user can run Firecracker but cannot create tap devices directly.
  Evidence: `ip tuntap add name openomanprobe mode tap` returned `ioctl(TUNSETIFF): Operation not permitted`.
- Observation: the built Alpine rootfs already contains BusyBox `ip`/`udhcpc` applets, so guest-side IP setup did not require a rootfs package-set change.
  Evidence: `debugfs` listing of `guest/out/rootfs.ext4` showed `busybox`, `iproute`, `ifconfig`, `route`, and `udhcpc`.
- Observation: for the selected proxy-only egress model, the guest does not need DHCP, DNS, or a default route in the first working implementation.
  Evidence: the host proxy URL can use the host tap IPv4 address directly, so only same-subnet connectivity is required.

## Decision Log

- Decision: implement only `host-proxy` networking now, not general NAT.
  Rationale: the repository’s design docs prefer a small, allowlisted proxy path, and the observed Codex workload only needs HTTPS to `api.openai.com`.
  Date/Author: 2026-03-02 / Codex
- Decision: use static `/30` guest addressing rather than DHCP.
  Rationale: it removes `dnsmasq` and default-route complexity while remaining sufficient for a host-local proxy endpoint.
  Date/Author: 2026-03-02 / Codex
- Decision: use a hidden `openoman internal firecracker-net` helper invoked through `sudo` instead of making the whole run path root.
  Rationale: tap lifecycle needs host network privileges, but the rest of the CLI and sandbox orchestration should remain unprivileged.
  Date/Author: 2026-03-02 / Codex
- Decision: enforce exact hostname allowlisting and HTTPS CONNECT only in the first proxy implementation.
  Rationale: this keeps the host proxy small, auditable, and aligned with the current `agent.egress_allowed_domains` shape.
  Date/Author: 2026-03-02 / Codex

## Outcomes & Retrospective

The repository now has a complete host-proxy networking path in code: new sandbox config types, CLI config parsing, hidden helper subcommands for tap lifecycle, deterministic per-run network leases, Firecracker NIC JSON emission, guest static IP/proxy configuration, and appended host-network logs in the collected `sandbox.logs` artifact. Unit and CLI tests cover the new configuration and lifecycle pieces.

The remaining gap is real end-to-end validation against `api.openai.com` from this host. That requires actual tap creation, so it depends on interactive `sudo` credentials that were not available from the current execution environment. The feature is implemented and locally testable up to the privilege boundary, but one manual repro is still needed to close the loop on live Codex connectivity.

## Context and Orientation

The direct Firecracker backend lives in `crates/core/src/sandbox/firecracker.rs`. That file stages the runtime image, emits the Firecracker JSON, starts the VM, waits for completion, and collects the modified workspace plus report/log output. Before this change it only emitted block devices and entropy, so the guest had no NIC.

The CLI entrypoint lives in `crates/cli/src/main.rs`. It parses `config.toml`, validates the sandbox backend at startup, submits jobs, and executes `openoman run <job_id>`. This change adds the hidden helper path there because tap lifecycle management must happen through the same executable with strict argument validation.

The guest boot contract lives in `guest/openoman-init.sh`. That script mounts `/dev/vdb`, loads `openoman-config/agent.env`, prepares the workspace, runs Codex, and writes logs and completion markers back into the runtime image. This change extends that contract with optional `OPENOMAN_NET_*` variables for static guest IP setup and host-proxy export.

## Plan of Work

First, extend the core sandbox contract so `FirecrackerBackendConfig` includes a `FirecrackerNetworkingConfig` with `mode`, `privilege_mode`, `tap_name_prefix`, `proxy_port`, and `subnet_cidr`. Then update the CLI config loader to parse `[sandbox.firecracker.network]`, default networking to `disabled`, and reject invalid host-proxy configurations such as an empty allowlist or a user-supplied `agent.egress_proxy_url`.

Next, add hidden CLI subcommands that perform the only privileged host actions needed for the MVP: create/delete a tap device and assign the host-side IPv4 address. The normal `run` path primes `sudo` credentials once with `sudo -v` and the direct runner later calls the helper non-interactively through `sudo -n`.

Then, update the direct Firecracker runner to allocate a deterministic `/30` subnet from the configured pool, create the tap, start a small host-local CONNECT proxy bound to the host tap IP, and emit a Firecracker `network-interfaces` section when networking mode is `host-proxy`. The runtime env file must carry `OPENOMAN_NET_MODE`, `OPENOMAN_NET_IFACE`, `OPENOMAN_NET_GUEST_IPV4`, and `OPENOMAN_NET_HOST_PROXY_URL`, plus the effective `HTTP_PROXY` and `HTTPS_PROXY`.

Finally, update `guest/openoman-init.sh` so it configures `eth0` statically before Codex startup, exports the proxy URL, and logs the network state. Ensure collected sandbox logs append the host-side `network.log` so users can see allow/deny decisions and proxy failures.

## Concrete Steps

From the repository root:

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    CARGO_HOME=/tmp/openoman-cargo-home \
    cargo test -p openoman-core firecracker -- --nocapture

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    CARGO_HOME=/tmp/openoman-cargo-home \
    cargo test -p openoman-cli -- --nocapture

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    ./guest/build-rootfs.sh ./guest/out

For a manual repro once `sudo` credentials are available:

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    cargo build -p openoman-cli

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    ./target/debug/openoman submit \
      --repo git@github.com:antonguzun/my_repo.git \
      --revision main \
      --instruction 'add 12345 into end of README'

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    ./target/debug/openoman run <job_id>

    env PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin \
    ./target/debug/openoman logs <job_id>

Expected observable behavior after the manual repro:

    - `openoman run` prompts for `sudo` once if the ticket is not already cached.
    - guest logs mention `configuring guest network` and `egress proxy configured`.
    - appended host network logs show `proxy accepted api.openai.com:443`.
    - the resulting job no longer fails before `codex exec` due to missing guest networking.

## Validation and Acceptance

Acceptance is met when:

1. `config.toml` can express `host-proxy` networking and the CLI rejects invalid combinations at load time.
2. Firecracker JSON includes one `network-interfaces` entry when networking is enabled.
3. The guest receives static networking env vars and logs its `eth0` configuration before Codex startup.
4. The host proxy denies non-allowlisted hostnames and records those denials in logs.
5. The collected `sandbox.logs` artifact includes both guest output and host network output.
6. A real `openoman run` can reach `https://api.openai.com/v1/responses` through the host proxy once `sudo` credentials are available.

## Idempotence and Recovery

The direct runner uses deterministic tap names derived from the Firecracker handle ID, so helper setup always deletes any stale tap of the same name before recreating it. Helper teardown is idempotent and succeeds even when the tap is already absent. Per-run runtime directories remain disposable under `workspaces/sandboxes/runs/`; if a run fails before cleanup, rerunning `openoman run` with a new attempt or cleaning the stale tap manually is safe.

Rebuilding the rootfs with `./guest/build-rootfs.sh ./guest/out` is safe to repeat because the script recreates `guest/out/rootfs.ext4` from scratch.

## Artifacts and Notes

The most useful runtime evidence after this change is in `sandbox.logs`, which now includes a host-side section like:

    [host network log]
    network setup requested: tap=oomtap1 host_ip=172.22.0.1/30 guest_ip=172.22.0.2/30 proxy_port=3128 allowlist=api.openai.com
    running network helper: sudo -n /path/to/openoman internal firecracker-net setup --tap-name oomtap1 --host-ip 172.22.0.1 --prefix-len 30
    host proxy listening on 172.22.0.1:3128

When the proxy denies traffic, the host log records a line like:

    proxy denied example.com:443 from 172.22.0.2:NNNN: hostname not allowlisted

## Interfaces and Dependencies

Core sandbox types now include:

    pub struct FirecrackerNetworkingConfig {
        pub mode: FirecrackerNetworkingMode,
        pub privilege_mode: FirecrackerNetworkPrivilegeMode,
        pub tap_name_prefix: String,
        pub proxy_port: u16,
        pub subnet_cidr: String,
    }

    pub enum FirecrackerNetworkingMode {
        Disabled,
        HostProxy,
    }

    pub enum FirecrackerNetworkPrivilegeMode {
        Sudo,
        Direct,
    }

CLI config now supports:

    [sandbox.firecracker.network]
    mode = "host-proxy"
    privilege_mode = "sudo"
    tap_name_prefix = "oomtap"
    proxy_port = 3128
    subnet_cidr = "172.22.0.0/16"

Hidden helper CLI:

    openoman internal firecracker-net setup --tap-name <name> --host-ip <ipv4> --prefix-len <n>
    openoman internal firecracker-net teardown --tap-name <name>

Guest env additions:

    OPENOMAN_NET_MODE='host-proxy'
    OPENOMAN_NET_IFACE='eth0'
    OPENOMAN_NET_GUEST_IPV4='172.22.0.2/30'
    OPENOMAN_NET_HOST_PROXY_URL='http://172.22.0.1:3128'

Dependencies introduced by the feature:

- host `ip`
- host `sudo` when `privilege_mode = "sudo"`
- Firecracker NIC support in the guest kernel (`CONFIG_VIRTIO_NET=y`, already satisfied by the pinned `firecracker-ci` kernel)
- BusyBox `ip` inside the guest rootfs (already present in the Alpine-based guest image)

Revision note (2026-03-02): Created this ExecPlan during implementation to document the completed host-proxy networking feature, the host privilege constraint, and the remaining manual end-to-end validation step.
