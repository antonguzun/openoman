# Firecracker guest assets

The direct Firecracker backend expects two guest-side assets:

- a Linux kernel image exposed through `sandbox.firecracker.kernel_image_path`
- a root filesystem image exposed through `sandbox.firecracker.rootfs_image_path`

This directory provides the guest init script used by the current MVP contract plus helper scripts to prepare a usable Firecracker asset pair without needing host root mounts.

## Current contract

The host runtime boots Firecracker with:

- a per-attempt writable copy of the configured root filesystem as the root block device (`/dev/vda`)
- a per-attempt ext4 runtime image as `/dev/vdb`
- optionally, a tap-backed virtio-net interface when `sandbox.firecracker.network.mode = "host-proxy"`
- kernel boot args that set `init=/sbin/openoman-init`

The rootfs therefore needs to contain `/sbin/openoman-init`. The checked-in [`openoman-init.sh`](/home/antonguzun/Work/personal/openoman/guest/openoman-init.sh) implements the current guest contract:

- mount `/dev/vdb` at `/mnt/runtime`
- load `/mnt/runtime/openoman-config/agent.env`
- initialize guest loopback so `127.0.0.1` is available to agent and Docker workloads
- optionally copy a staged provider auth file into the location expected by the selected CLI
- optionally start `dockerd` inside the guest when Firecracker config enables guest Docker daemon support, with Docker data rooted at `/mnt/runtime/docker`
- bind configured package directories into the guest filesystem
- optionally configure `eth0` with a static `/30` address and export a host-local HTTP CONNECT proxy
- run the agent inside `/mnt/runtime/workspace` with Firecracker as the outer sandbox boundary
- write logs and report into `/mnt/runtime/openoman-output`
- write an exit-code marker into `/mnt/runtime/openoman-output/exit-code.txt`
- attempt shutdown, while the host runner is also allowed to terminate Firecracker once that completion marker appears

## Build a kernel plus rootfs pair

From the repository root:

```sh
chmod +x ./guest/build-assets.sh ./guest/build-rootfs.sh ./guest/download-firecracker-kernel.sh
./guest/build-assets.sh ./guest/out
```

The asset build flow requires:

- `curl`
- `mkfs.ext4`
- `tar`
- optionally `file` for a local type check on the downloaded kernel
- either `podman` or `docker`

Set `OPENOMAN_CONTAINER_ENGINE=docker` or `OPENOMAN_CONTAINER_ENGINE=podman` if you want to force a specific container engine for the rootfs build.

`build-assets.sh` does two things:

- downloads an uncompressed Firecracker-compatible kernel into `./guest/out/vmlinux`
- downloads the matching published kernel config into `./guest/out/vmlinux.config`
- builds a root filesystem image with `openoman-init.sh` injected as `/sbin/openoman-init` into `./guest/out/rootfs.ext4`
- by default, installs guest-side `make`, Docker CLI, `dockerd`, `docker compose` support, `python3`, a `python -> python3` compatibility symlink, `node`, `npm`, `git`, `ripgrep`, `@openai/codex`, Cursor's `cursor-agent`, and Anthropic's `claude` (Claude Code), so common test/agent CLIs exist inside the guest image without host bind mounts

The default kernel download URL now points at Firecracker's official `firecracker-ci` guest kernel for the local architecture. The script also downloads the published sidecar config and refuses the build if it does not contain `CONFIG_HW_RANDOM_VIRTIO=y`, because the current guest Node/Codex workload stalls without that driver.

On `x86_64`, the default is `https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/x86_64/vmlinux-6.1.128`. On `aarch64`, the default is `https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/aarch64/vmlinux-6.1.128`. Override `OPENOMAN_FIRECRACKER_KERNEL_URL` if you want a different kernel binary, and override `OPENOMAN_FIRECRACKER_KERNEL_CONFIG_URL` if the matching `.config` lives somewhere else.

If you need a different guest package set, override the container setup step:

```sh
OPENOMAN_GUEST_SETUP_CMD='apk add --no-cache bash curl nodejs npm && npm install -g @openai/codex && curl -fsSL https://cursor.com/install | bash && curl -fsSL https://claude.ai/install.sh | bash'
./guest/build-rootfs.sh ./guest/out

If you need private Docker registry access inside the guest, configure it on the host side through `sandbox.firecracker.docker_auth_config` or `sandbox.firecracker.docker_auth_config_env`. At run time, openoman stages that explicit Docker config into `/root/.docker/config.json`. No host Docker login is copied automatically.

If you also need a Docker daemon inside the guest, set `sandbox.firecracker.docker_daemon = true`. That makes `openoman-init.sh` start `dockerd` during boot and wait until `docker info` succeeds before the agent begins running. Docker images, layers, and build cache are stored under `/mnt/runtime/docker`, so they consume the per-attempt runtime disk instead of the shared guest rootfs. If a Docker-heavy workflow needs more room, set `sandbox.firecracker.runtime_disk_mb` to enlarge `/dev/vdb` for that attempt. This is separate from Docker registry auth: auth controls credentials, while `docker_daemon` controls whether the guest starts a local daemon at all.
```

If you only want the rootfs image, run:

```sh
./guest/build-rootfs.sh ./guest/out
```

If you only want to download the default kernel plus its matching config, run:

```sh
./guest/download-firecracker-kernel.sh ./guest/out
```

## Opt-in real Firecracker smoke test

Once `./guest/out/vmlinux`, `./guest/out/vmlinux.config`, and `./guest/out/rootfs.ext4` exist, run:

```sh
OPENOMAN_FIRECRACKER_E2E=1 cargo test -p openoman-core direct_runner_smoke_boots_real_firecracker_when_opted_in -- --nocapture
```

The smoke test uses the real Firecracker binary, boots a VM with the built asset pair, stages a tiny shell-based fake `codex` into the guest through `user_package_dirs`, and verifies that the guest modifies the workspace and writes report/log output.

Override paths if needed:

```sh
OPENOMAN_FIRECRACKER_E2E=1 \
OPENOMAN_FIRECRACKER_BIN="$HOME/.local/bin/firecracker" \
OPENOMAN_FIRECRACKER_KERNEL="/abs/path/vmlinux" \
OPENOMAN_FIRECRACKER_ROOTFS="/abs/path/rootfs.ext4" \
cargo test -p openoman-core direct_runner_smoke_boots_real_firecracker_when_opted_in -- --nocapture
```

## Host-proxy networking

Direct Firecracker mode now supports a host-proxy networking path for real agent/API traffic:

- `sandbox.firecracker.network.mode = "host-proxy"`
- the host creates one tap device per run
- the guest gets a static `eth0` address inside a `/30` subnet
- `HTTP_PROXY` and `HTTPS_PROXY` inside the guest point at a host-local HTTP CONNECT proxy
- the proxy allows only exact hostnames from `agent.egress_allowed_domains`
- CONNECT destinations are further restricted to `sandbox.firecracker.network.allowed_connect_ports`, which defaults to `[443]`

This keeps guest egress constrained to the host proxy path instead of giving the VM general outbound internet access.

`privilege_mode = "sudo"` is the default because creating the tap device requires host network privileges. `openoman run` calls `sudo -v` once before the attempt starts, then uses a hidden internal helper for tap setup and teardown. Use `privilege_mode = "direct"` only when the whole `openoman` process already runs with the required capabilities.

The host proxy defaults to HTTPS CONNECT on port `443` only. If your guest needs to reach a private HTTPS service on another port, such as a Docker registry on `:5050`, add that port to `sandbox.firecracker.network.allowed_connect_ports`.

The current guest kernel does not enable Linux Landlock (`CONFIG_SECURITY_LANDLOCK` is off in the pinned Firecracker kernel config). The Codex branch therefore runs `codex exec` with `--dangerously-bypass-approvals-and-sandbox`, and the Cursor branch relies on Firecracker itself rather than an inner sandbox. That is intentional here: Firecracker is already the actual isolation boundary for the untrusted workload.

## Limitations

- host-proxy mode currently supports HTTPS through HTTP CONNECT only, enforces a minimum of TLS 1.2 for tunneled connections, and does not provide general guest internet access.
- `agent.egress_allowed_domains` uses exact hostname matching; wildcard domains are not implemented.
- guest networking in direct mode depends on host privileges for tap lifecycle management.
- user package directories are copied into the per-run runtime image; they are not live-mounted from the host.
- copied host-user binaries must still be compatible with the guest userspace to run successfully.
- `sandbox.firecracker.mode = "jailer"` is not implemented yet and fails during startup validation.
