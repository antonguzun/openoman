# Firecracker guest assets

The direct Firecracker backend expects two guest-side assets:

- a Linux kernel image exposed through `sandbox.firecracker.kernel_image_path`
- a root filesystem image exposed through `sandbox.firecracker.rootfs_image_path`

This directory provides the guest init script used by the current MVP contract plus helper scripts to prepare a usable Firecracker asset pair without needing host root mounts.

## Current contract

The host runtime boots Firecracker with:

- a per-attempt writable copy of the configured root filesystem as the root block device (`/dev/vda`)
- a per-attempt ext4 runtime image as `/dev/vdb`
- kernel boot args that set `init=/sbin/openoman-init`

The rootfs therefore needs to contain `/sbin/openoman-init`. The checked-in [`openoman-init.sh`](/home/antonguzun/Work/personal/openoman/guest/openoman-init.sh) implements the current guest contract:

- mount `/dev/vdb` at `/mnt/runtime`
- load `/mnt/runtime/openoman-config/agent.env`
- bind configured package directories into the guest filesystem
- run the agent inside `/mnt/runtime/workspace`
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
- by default, installs guest-side `node`, `npm`, `git`, `ripgrep`, and `@openai/codex`, so `/usr/local/bin/codex` exists inside the guest image without host bind mounts

The default kernel download URL now points at Firecracker's official `firecracker-ci` guest kernel for the local architecture. The script also downloads the published sidecar config and refuses the build if it does not contain `CONFIG_HW_RANDOM_VIRTIO=y`, because the current guest Node/Codex workload stalls without that driver.

On `x86_64`, the default is `https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/x86_64/vmlinux-6.1.128`. On `aarch64`, the default is `https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/aarch64/vmlinux-6.1.128`. Override `OPENOMAN_FIRECRACKER_KERNEL_URL` if you want a different kernel binary, and override `OPENOMAN_FIRECRACKER_KERNEL_CONFIG_URL` if the matching `.config` lives somewhere else.

If you need a different guest package set, override the container setup step:

```sh
OPENOMAN_GUEST_SETUP_CMD='apk add --no-cache nodejs npm && npm install -g @openai/codex'
./guest/build-rootfs.sh ./guest/out
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

## Limitations

- `sandbox.firecracker.mode = "direct"` currently does not set up a guest network device.
- the default `firecracker-ci` kernel solves the earlier virtio-rng gap, but real `codex exec` still needs future guest networking support.
- a built-in `codex` binary avoids the earlier "binary not found" failure, but real `codex exec` still needs future guest networking support.
- user package directories are copied into the per-run runtime image; they are not live-mounted from the host.
- copied host-user binaries must still be compatible with the guest userspace to run successfully.
- `sandbox.firecracker.mode = "jailer"` is not implemented yet and fails during startup validation.
