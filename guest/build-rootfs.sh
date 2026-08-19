#!/usr/bin/env sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <output-dir>" >&2
  exit 2
fi

OUTPUT_DIR="$1"
ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
INIT_SCRIPT="$ROOT_DIR/openoman-init.sh"
DOCKER_WRAPPER_SCRIPT="$ROOT_DIR/docker-compose-wrapper.sh"

if [ ! -f "$INIT_SCRIPT" ]; then
  echo "missing guest init script: $INIT_SCRIPT" >&2
  exit 1
fi

if [ ! -f "$DOCKER_WRAPPER_SCRIPT" ]; then
  echo "missing docker wrapper script: $DOCKER_WRAPPER_SCRIPT" >&2
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

IMAGE_REF="${OPENOMAN_GUEST_IMAGE:-debian:bookworm-slim}"
GUEST_SETUP_CMD="${OPENOMAN_GUEST_SETUP_CMD:-apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends bash ca-certificates curl docker-compose docker.io git iproute2 iptables make nodejs npm python3 python3-pip python3-venv ripgrep strace && if command -v iptables-legacy >/dev/null 2>&1; then update-alternatives --set iptables \$(command -v iptables-legacy); fi && if command -v ip6tables-legacy >/dev/null 2>&1; then update-alternatives --set ip6tables \$(command -v ip6tables-legacy); fi && ln -sf \$(command -v python3) /usr/local/bin/python && npm install -g @openai/codex && curl -fsSL https://cursor.com/install | bash && curl -fsSL https://claude.ai/install.sh | bash && export PATH=/usr/local/bin:\$HOME/.local/bin:\$PATH && command -v make >/dev/null && command -v docker >/dev/null && command -v dockerd >/dev/null && command -v docker-compose >/dev/null && command -v iptables >/dev/null && iptables --version | grep -q legacy && command -v python3 >/dev/null && command -v python >/dev/null && command -v node >/dev/null && command -v codex >/dev/null && command -v cursor-agent >/dev/null && command -v claude >/dev/null && command -v strace >/dev/null && command -v ip >/dev/null && apt-get clean && rm -rf /var/lib/apt/lists/*}"
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
  "$ROOTFS_TREE/sbin" \
  "$ROOTFS_TREE/usr/local/bin"
cp "$INIT_SCRIPT" "$ROOTFS_TREE/sbin/openoman-init"
chmod 755 "$ROOTFS_TREE/sbin/openoman-init"
cp "$DOCKER_WRAPPER_SCRIPT" "$ROOTFS_TREE/usr/local/bin/docker"
chmod 755 "$ROOTFS_TREE/usr/local/bin/docker"

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
