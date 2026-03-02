#!/usr/bin/env sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <output-dir>" >&2
  exit 2
fi

OUTPUT_DIR="$1"
ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
INIT_SCRIPT="$ROOT_DIR/openoman-init.sh"

if [ ! -f "$INIT_SCRIPT" ]; then
  echo "missing guest init script: $INIT_SCRIPT" >&2
  exit 1
fi

for cmd in mkfs.ext4 tar; do
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "required command not found: $cmd" >&2
    exit 1
  }
done

CONTAINER_ENGINE="${OPENOMAN_CONTAINER_ENGINE:-}"
if [ -n "$CONTAINER_ENGINE" ]; then
  command -v "$CONTAINER_ENGINE" >/dev/null 2>&1 || {
    echo "requested container engine not found: $CONTAINER_ENGINE" >&2
    exit 1
  }
else
  for candidate in docker podman; do
    if command -v "$candidate" >/dev/null 2>&1; then
      CONTAINER_ENGINE="$candidate"
      break
    fi
  done
fi

if [ -z "$CONTAINER_ENGINE" ]; then
  echo "podman or docker is required to build the guest rootfs" >&2
  exit 1
fi

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
ROOTFS_TREE="$TMPDIR/rootfs-tree"
mkdir -p "$ROOTFS_TREE" "$OUTPUT_DIR"

IMAGE_REF="${OPENOMAN_GUEST_IMAGE:-alpine:3.20}"
GUEST_SETUP_CMD="${OPENOMAN_GUEST_SETUP_CMD:-apk add --no-cache bash ca-certificates git nodejs npm ripgrep strace && npm install -g @openai/codex && command -v node >/dev/null && command -v codex >/dev/null && command -v strace >/dev/null}"
CONTAINER_ID="$($CONTAINER_ENGINE create "$IMAGE_REF" sh -c "$GUEST_SETUP_CMD")"
trap '$CONTAINER_ENGINE rm -f "$CONTAINER_ID" >/dev/null 2>&1 || true; rm -rf "$TMPDIR"' EXIT

$CONTAINER_ENGINE start -a "$CONTAINER_ID"
$CONTAINER_ENGINE export "$CONTAINER_ID" | tar -C "$ROOTFS_TREE" -xf -
$CONTAINER_ENGINE rm -f "$CONTAINER_ID" >/dev/null 2>&1 || true

mkdir -p \
  "$ROOTFS_TREE/dev" \
  "$ROOTFS_TREE/proc" \
  "$ROOTFS_TREE/sys" \
  "$ROOTFS_TREE/tmp" \
  "$ROOTFS_TREE/mnt/runtime" \
  "$ROOTFS_TREE/sbin"
cp "$INIT_SCRIPT" "$ROOTFS_TREE/sbin/openoman-init"
chmod 755 "$ROOTFS_TREE/sbin/openoman-init"

ROOTFS_IMAGE="$OUTPUT_DIR/rootfs.ext4"
rm -f "$ROOTFS_IMAGE"
ROOTFS_BYTES="$(du -sk "$ROOTFS_TREE" | awk '{print $1 * 1024}')"
EXTRA_BYTES="$((128 * 1024 * 1024))"
IMAGE_BYTES="$((ROOTFS_BYTES + ROOTFS_BYTES / 4 + EXTRA_BYTES))"
BLOCK_SIZE=4096
BLOCKS="$(((IMAGE_BYTES + BLOCK_SIZE - 1) / BLOCK_SIZE))"
mkfs.ext4 -F -b "$BLOCK_SIZE" -d "$ROOTFS_TREE" "$ROOTFS_IMAGE" "$BLOCKS" >/dev/null

echo "rootfs image created at $ROOTFS_IMAGE"
echo "configure sandbox.firecracker.rootfs_image_path to this file"
