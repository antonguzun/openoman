#!/usr/bin/env sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <output-dir>" >&2
  exit 2
fi

OUTPUT_DIR="$1"
ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

"$ROOT_DIR/download-firecracker-kernel.sh" "$OUTPUT_DIR"
"$ROOT_DIR/build-rootfs.sh" "$OUTPUT_DIR"

cat <<EOF

firecracker assets prepared:
  kernel: $OUTPUT_DIR/vmlinux
  rootfs: $OUTPUT_DIR/rootfs.ext4

example config:

[sandbox.firecracker]
mode = "direct"
kernel_image_path = "$OUTPUT_DIR/vmlinux"
rootfs_image_path = "$OUTPUT_DIR/rootfs.ext4"
EOF
