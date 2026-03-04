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
echo "agent bin: ${AGENT_BIN:-}"
if [ -n "${AGENT_MODEL:-}" ]; then
  echo "agent model: $AGENT_MODEL"
fi
export HOME="/root"
export PATH="$HOME/.local/bin:$PATH"

redact_secret() {
  secret_value="${1:-}"
  if [ -z "$secret_value" ]; then
    printf "<empty>"
    return 0
  fi

  secret_length=${#secret_value}
  if [ "$secret_length" -le 8 ]; then
    printf "<redacted:%s chars>" "$secret_length"
    return 0
  fi

  prefix_length=6
  suffix_length=4
  if [ "$secret_length" -le $((prefix_length + suffix_length)) ]; then
    prefix_length=2
    suffix_length=2
  fi

  prefix_value="$(printf "%s" "$secret_value" | cut -c1-"$prefix_length")"
  suffix_start=$((secret_length - suffix_length + 1))
  suffix_value="$(printf "%s" "$secret_value" | cut -c"$suffix_start"-"$secret_length")"
  printf "%s...%s" "$prefix_value" "$suffix_value"
}

quote_log_arg() {
  printf "'"
  printf "%s" "$1" | sed "s/'/'\\\\''/g"
  printf "'"
}

log_command() {
  log_label="$1"
  shift
  printf "%s" "$log_label"
  separator=""
  while [ "$#" -gt 0 ]; do
    printf "%s" "$separator"
    quote_log_arg "$1"
    separator=" "
    shift
  done
  printf "\n"
}

log_command_argv() {
  log_label="$1"
  shift
  echo "$log_label"
  arg_index=0
  while [ "$#" -gt 0 ]; do
    printf "  argv[%s]=" "$arg_index"
    quote_log_arg "$1"
    printf "\n"
    arg_index=$((arg_index + 1))
    shift
  done
}

log_command_redacting_api_key() {
  log_label="$1"
  shift
  printf "%s" "$log_label"
  separator=""
  redact_next=0
  while [ "$#" -gt 0 ]; do
    printf "%s" "$separator"
    if [ "$redact_next" -eq 1 ]; then
      quote_log_arg "$(redact_secret "$1")"
      redact_next=0
    else
      quote_log_arg "$1"
      if [ "$1" = "--api-key" ]; then
        redact_next=1
      fi
    fi
    separator=" "
    shift
  done
  printf "\n"
}

log_command_argv_redacting_api_key() {
  log_label="$1"
  shift
  echo "$log_label"
  arg_index=0
  redact_next=0
  while [ "$#" -gt 0 ]; do
    printf "  argv[%s]=" "$arg_index"
    if [ "$redact_next" -eq 1 ]; then
      quote_log_arg "$(redact_secret "$1")"
      redact_next=0
    else
      quote_log_arg "$1"
      if [ "$1" = "--api-key" ]; then
        redact_next=1
      fi
    fi
    printf "\n"
    arg_index=$((arg_index + 1))
    shift
  done
}

if command -v ip >/dev/null 2>&1; then
  OPENOMAN_IP_BIN="$(command -v ip)"
elif command -v busybox >/dev/null 2>&1; then
  OPENOMAN_IP_BIN="busybox ip"
else
  echo "ip command not found" >&2
  exit 1
fi

if [ "$AGENT_PROVIDER" = "codex" ] && [ -n "${OPENOMAN_AGENT_AUTH_FILE:-}" ]; then
  echo "installing codex auth file from $OPENOMAN_AGENT_AUTH_FILE"
  mkdir -p /root/.codex
  cp "$OPENOMAN_AGENT_AUTH_FILE" /root/.codex/auth.json
  chmod 600 /root/.codex/auth.json
fi

if [ "$AGENT_PROVIDER" = "cursor" ] && [ -n "${OPENOMAN_AGENT_AUTH_FILE:-}" ]; then
  echo "installing cursor auth file from $OPENOMAN_AGENT_AUTH_FILE"
  mkdir -p /root/.config/cursor
  cp "$OPENOMAN_AGENT_AUTH_FILE" /root/.config/cursor/auth.json
  chmod 600 /root/.config/cursor/auth.json
fi

if [ "${OPENOMAN_NET_MODE:-}" = "host-proxy" ]; then
  echo "configuring guest network: iface=${OPENOMAN_NET_IFACE:-eth0} addr=${OPENOMAN_NET_GUEST_IPV4:-unset}"
  sh -c "$OPENOMAN_IP_BIN link set \"${OPENOMAN_NET_IFACE:-eth0}\" up"
  sh -c "$OPENOMAN_IP_BIN addr flush dev \"${OPENOMAN_NET_IFACE:-eth0}\"" || true
  sh -c "$OPENOMAN_IP_BIN addr add \"${OPENOMAN_NET_GUEST_IPV4:?missing guest ipv4}\" dev \"${OPENOMAN_NET_IFACE:-eth0}\""
  if [ "${OPENOMAN_NET_ALLOW_ALL:-0}" = "1" ]; then
    echo "host-proxy debug mode: enabling default route and guest DNS"
    sh -c "$OPENOMAN_IP_BIN route replace default via \"${OPENOMAN_NET_HOST_IPV4:?missing host ipv4}\" dev \"${OPENOMAN_NET_IFACE:-eth0}\""
    if [ -n "${OPENOMAN_NET_DNS_SERVERS:-}" ]; then
      : > /etc/resolv.conf
      OLD_IFS="$IFS"
      IFS=','
      for dns_server in $OPENOMAN_NET_DNS_SERVERS; do
        echo "nameserver $dns_server" >> /etc/resolv.conf
      done
      IFS="$OLD_IFS"
      echo "guest DNS servers: $OPENOMAN_NET_DNS_SERVERS"
    else
      echo "guest DNS servers unavailable; /etc/resolv.conf unchanged"
    fi
  fi
  export HTTPS_PROXY="${OPENOMAN_NET_HOST_PROXY_URL:?missing host proxy url}"
  export HTTP_PROXY="$HTTPS_PROXY"
  sh -c "$OPENOMAN_IP_BIN addr show dev \"${OPENOMAN_NET_IFACE:-eth0}\"" || true
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
    if command -v "$AGENT_BIN" >/dev/null 2>&1; then
      resolved_codex="$(command -v "$AGENT_BIN" || true)"
      echo "resolved codex path: ${resolved_codex:-$AGENT_BIN}"
      codex_target="$(readlink -f "$AGENT_BIN" 2>/dev/null || true)"
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
        if timeout -k 1 10 "$AGENT_BIN" --version; then
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
        "$AGENT_BIN" exec --dangerously-bypass-approvals-and-sandbox --color never -C "$WORKSPACE_DIR" -o "$OUTPUT_DIR/report.txt" "$instruction" || agent_status=$?
        echo "codex exec exit status: $agent_status"
      fi
    else
      echo "codex binary not found: $AGENT_BIN"
      agent_status=127
    fi
    ;;
  cursor)
    echo "checking cursor availability"
    if [ -z "${CURSOR_API_KEY:-}" ]; then
      if [ -z "${OPENOMAN_AGENT_AUTH_FILE:-}" ]; then
        echo "CURSOR_API_KEY is not set and no staged cursor auth file is available; skipping cursor exec"
        agent_status=127
      fi
    fi
    if [ "$agent_status" -eq 0 ] && command -v "$AGENT_BIN" >/dev/null 2>&1; then
      resolved_cursor="$(command -v "$AGENT_BIN" || true)"
      echo "resolved cursor path: ${resolved_cursor:-$AGENT_BIN}"
      echo "running timed cursor --version"
      if timeout -k 1 10 "$AGENT_BIN" --version; then
        cursor_version_status=0
      else
        cursor_version_status=$?
      fi
        echo "timed cursor --version exit status: $cursor_version_status"
      if [ "$cursor_version_status" -ne 0 ]; then
        echo "cursor --version failed; skipping cursor exec"
        agent_status=$cursor_version_status
      else
        cursor_auth_mode=""
        echo "running cursor print mode in $WORKSPACE_DIR"
        if [ -n "${OPENOMAN_AGENT_AUTH_FILE:-}" ]; then
          cursor_auth_mode="staged auth file"
        else
          cursor_auth_mode="api key"
        fi
        set -- "$AGENT_BIN"
        if [ "$cursor_auth_mode" = "api key" ]; then
          set -- "$@" --api-key "$CURSOR_API_KEY"
        fi
        set -- "$@" -p -f --output-format text
        if [ -n "${AGENT_MODEL:-}" ]; then
          set -- "$@" --model "$AGENT_MODEL"
        fi
        set -- "$@" "$instruction"
        echo "cursor auth mode: $cursor_auth_mode"
        log_command_redacting_api_key "cursor command: " "$@"
        log_command_argv_redacting_api_key "cursor command argv:" "$@"
        (
          cd "$WORKSPACE_DIR"
          if [ "$cursor_auth_mode" = "staged auth file" ]; then
            unset CURSOR_API_KEY || true
          fi
          "$@"
        ) > "$OUTPUT_DIR/report.txt" || agent_status=$?
        echo "cursor exec exit status: $agent_status"
      fi
    else
      echo "cursor binary not found: $AGENT_BIN"
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
