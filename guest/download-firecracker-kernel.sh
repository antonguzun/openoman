#!/usr/bin/env sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <output-dir>" >&2
  exit 2
fi

OUTPUT_DIR="$1"
mkdir -p "$OUTPUT_DIR"

command -v curl >/dev/null 2>&1 || {
  echo "required command not found: curl" >&2
  exit 1
}

ARCH="$(uname -m)"
DEFAULT_URL=""
DEFAULT_CONFIG_URL=""
case "$ARCH" in
  x86_64|amd64)
    DEFAULT_URL="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/x86_64/vmlinux-6.1.128"
    DEFAULT_CONFIG_URL="${DEFAULT_URL}.config"
    ;;
  aarch64|arm64)
    DEFAULT_URL="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/aarch64/vmlinux-6.1.128"
    DEFAULT_CONFIG_URL="${DEFAULT_URL}.config"
    ;;
  *)
    echo "unsupported architecture '$ARCH'; set OPENOMAN_FIRECRACKER_KERNEL_URL explicitly" >&2
    exit 1
    ;;
esac

KERNEL_URL="${OPENOMAN_FIRECRACKER_KERNEL_URL:-$DEFAULT_URL}"
KERNEL_CONFIG_URL="${OPENOMAN_FIRECRACKER_KERNEL_CONFIG_URL:-${KERNEL_URL}.config}"
KERNEL_PATH="$OUTPUT_DIR/vmlinux"
KERNEL_CONFIG_PATH="$OUTPUT_DIR/vmlinux.config"

curl --fail --location --silent --show-error "$KERNEL_URL" -o "$KERNEL_PATH"
curl --fail --location --silent --show-error "$KERNEL_CONFIG_URL" -o "$KERNEL_CONFIG_PATH"

if ! rg -qx 'CONFIG_HW_RANDOM_VIRTIO=y' "$KERNEL_CONFIG_PATH"; then
  echo "downloaded kernel config at $KERNEL_CONFIG_PATH does not enable CONFIG_HW_RANDOM_VIRTIO=y" >&2
  exit 1
fi

if command -v file >/dev/null 2>&1; then
  file "$KERNEL_PATH" || true
fi

echo "kernel image downloaded to $KERNEL_PATH"
echo "source url: $KERNEL_URL"
echo "kernel config downloaded to $KERNEL_CONFIG_PATH"
echo "config source url: $KERNEL_CONFIG_URL"
