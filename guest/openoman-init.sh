#!/bin/sh
set -eu

mkdir -p /proc /sys /dev /tmp /mnt/runtime
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev || true
mount -t tmpfs tmpfs /tmp

runtime_device="/dev/vdb"
attempts=0
while [ ! -b "$runtime_device" ] && [ "$attempts" -lt 50 ]; do
  attempts=$((attempts + 1))
  sleep 0.1
done

if [ ! -b "$runtime_device" ]; then
  echo "runtime block device not found: $runtime_device" >&2
  exit 1
fi

mount "$runtime_device" /mnt/runtime

mkdir -p /mnt/runtime/openoman-output
exec > /mnt/runtime/openoman-output/logs.txt 2>&1

echo "openoman guest init started"

. /mnt/runtime/openoman-config/agent.env

echo "agent provider: $AGENT_PROVIDER"
echo "workspace dir: $WORKSPACE_DIR"
echo "output dir: $OUTPUT_DIR"
echo "codex bin: ${CODEX_BIN:-}"
export HOME="/root"

if [ -n "${OPENOMAN_CODEX_AUTH_FILE:-}" ]; then
  echo "installing codex auth file from $OPENOMAN_CODEX_AUTH_FILE"
  mkdir -p /root/.codex
  cp "$OPENOMAN_CODEX_AUTH_FILE" /root/.codex/auth.json
  chmod 600 /root/.codex/auth.json
fi

if [ "${OPENOMAN_NET_MODE:-}" = "host-proxy" ]; then
  echo "configuring guest network: iface=${OPENOMAN_NET_IFACE:-eth0} addr=${OPENOMAN_NET_GUEST_IPV4:-unset}"
  busybox ip link set "${OPENOMAN_NET_IFACE:-eth0}" up
  busybox ip addr flush dev "${OPENOMAN_NET_IFACE:-eth0}" || true
  busybox ip addr add "${OPENOMAN_NET_GUEST_IPV4:?missing guest ipv4}" dev "${OPENOMAN_NET_IFACE:-eth0}"
  export HTTPS_PROXY="${OPENOMAN_NET_HOST_PROXY_URL:?missing host proxy url}"
  export HTTP_PROXY="$HTTPS_PROXY"
  busybox ip addr show dev "${OPENOMAN_NET_IFACE:-eth0}" || true
fi

if [ -n "${HTTPS_PROXY:-}" ]; then
  export HTTPS_PROXY HTTP_PROXY
  echo "egress proxy configured: $HTTPS_PROXY"
fi

if [ -n "${OPENOMAN_EGRESS_ALLOWED_DOMAINS:-}" ]; then
  export OPENOMAN_EGRESS_ALLOWED_DOMAINS
  echo "egress allowed domains: $OPENOMAN_EGRESS_ALLOWED_DOMAINS"
fi

if [ -n "${OPENOMAN_EXTRA_PATH:-}" ]; then
  export PATH="$OPENOMAN_EXTRA_PATH:$PATH"
fi

if [ -f "$PACKAGE_MOUNTS_FILE" ]; then
  while IFS="$(printf '\t')" read -r source_path guest_path add_to_path; do
    [ -n "${source_path:-}" ] || continue
    echo "mounting package dir: $source_path -> $guest_path (add_to_path=$add_to_path)"
    mkdir -p "$guest_path"
    mount -o bind "$source_path" "$guest_path"
    if [ "$add_to_path" = "1" ]; then
      export PATH="$guest_path:$PATH"
    fi
  done < "$PACKAGE_MOUNTS_FILE"
fi

echo "effective PATH: $PATH"

instruction="$(cat "$INSTRUCTION_FILE")"
agent_status=0

case "$AGENT_PROVIDER" in
  codex)
    echo "checking codex availability"
    if command -v "$CODEX_BIN" >/dev/null 2>&1; then
      resolved_codex="$(command -v "$CODEX_BIN" || true)"
      echo "resolved codex path: ${resolved_codex:-$CODEX_BIN}"
      codex_target="$(readlink -f "$CODEX_BIN" 2>/dev/null || true)"
      echo "resolved codex target: ${codex_target:-unresolved}"
      codex_probe_failed=0
      echo "checking node availability"
      if command -v node >/dev/null 2>&1; then
        resolved_node="$(command -v node || true)"
        echo "resolved node path: ${resolved_node:-node}"
        echo "running node --version"
        node --version || true
        if [ -n "$codex_target" ] && [ -f "$codex_target" ]; then
          if command -v timeout >/dev/null 2>&1 && command -v strace >/dev/null 2>&1; then
            trace_prefix="$OUTPUT_DIR/node-codex-strace"
            rm -f "${trace_prefix}"*
            echo "running timed strace on node codex entrypoint --version"
            if timeout -k 1 10 strace -ff -tt -s 200 -o "$trace_prefix" node "$codex_target" --version; then
              probe_status=0
            else
              probe_status=$?
            fi
            echo "timed node codex probe exit status: $probe_status"
            for trace_file in "${trace_prefix}"*; do
              [ -f "$trace_file" ] || continue
              echo "trace tail: $trace_file"
              tail -n 40 "$trace_file" || true
            done
            if [ "$probe_status" -ne 0 ]; then
              echo "node codex entrypoint probe failed; skipping codex exec"
              agent_status=$probe_status
              codex_probe_failed=1
            fi
          fi
        fi
      else
        echo "node binary not found"
        agent_status=127
        codex_probe_failed=1
      fi
      if [ "$codex_probe_failed" -eq 0 ]; then
        echo "running timed codex --version"
        if timeout -k 1 10 "$CODEX_BIN" --version; then
          codex_version_status=0
        else
          codex_version_status=$?
        fi
        echo "timed codex --version exit status: $codex_version_status"
        if [ "$codex_version_status" -ne 0 ]; then
          echo "codex --version failed; skipping codex exec"
          agent_status=$codex_version_status
          codex_probe_failed=1
        fi
      fi
      if [ "$codex_probe_failed" -eq 0 ]; then
        echo "running codex exec in $WORKSPACE_DIR"
        "$CODEX_BIN" exec --dangerously-bypass-approvals-and-sandbox --color never -C "$WORKSPACE_DIR" -o "$OUTPUT_DIR/report.txt" "$instruction" || agent_status=$?
        echo "codex exec exit status: $agent_status"
      fi
    else
      echo "codex binary not found: $CODEX_BIN"
      agent_status=127
    fi
    ;;
  *)
    echo "unsupported agent provider: $AGENT_PROVIDER"
    agent_status=127
    ;;
esac

if [ ! -f "$OUTPUT_DIR/report.txt" ]; then
  echo "Sandbox execution completed without report output." > "$OUTPUT_DIR/report.txt"
fi

printf "%s\n" "$agent_status" > "$OUTPUT_DIR/exit-code.txt"
sync
echo "guest execution complete, attempting shutdown"
poweroff -f || halt -f || reboot -f || true

if [ -w /proc/sysrq-trigger ]; then
  echo o > /proc/sysrq-trigger || true
  sleep 1
  echo b > /proc/sysrq-trigger || true
fi

exit "$agent_status"
