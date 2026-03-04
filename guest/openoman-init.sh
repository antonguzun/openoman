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

echo "agent provider: ${OPENOMAN_AGENT_ID:-${AGENT_PROVIDER:-unknown}}"
echo "workspace dir: $WORKSPACE_DIR"
echo "output dir: $OUTPUT_DIR"
echo "agent bin: ${OPENOMAN_AGENT_BIN:-${AGENT_BIN:-}}"
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

if [ -n "${OPENOMAN_AGENT_AUTH_FILE:-}" ] && [ -n "${OPENOMAN_AGENT_AUTH_INSTALL_PATH:-}" ]; then
  echo "installing staged agent auth file to $OPENOMAN_AGENT_AUTH_INSTALL_PATH"
  install_dir="$(dirname "$OPENOMAN_AGENT_AUTH_INSTALL_PATH")"
  mkdir -p "$install_dir"
  cp "$OPENOMAN_AGENT_AUTH_FILE" "$OPENOMAN_AGENT_AUTH_INSTALL_PATH"
  chmod 600 "$OPENOMAN_AGENT_AUTH_INSTALL_PATH"
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

agent_status=0

agent_bin="${OPENOMAN_AGENT_BIN:-${AGENT_BIN:-}}"
if [ -z "$agent_bin" ]; then
  echo "agent binary is not configured"
  agent_status=127
elif ! command -v "$agent_bin" >/dev/null 2>&1; then
  echo "agent binary not found: $agent_bin"
  agent_status=127
else
  resolved_agent="$(command -v "$agent_bin" || true)"
  echo "resolved agent path: ${resolved_agent:-$agent_bin}"
fi

if [ "$agent_status" -eq 0 ]; then
  version_arg_count="${OPENOMAN_AGENT_VERSION_ARG_COUNT:-0}"
  set -- "$agent_bin"
  index=0
  while [ "$index" -lt "$version_arg_count" ]; do
    index_key="$(printf '%03d' "$index")"
    eval "arg_value=\${OPENOMAN_AGENT_VERSION_ARG_${index_key}:-}"
    set -- "$@" "$arg_value"
    index=$((index + 1))
  done
  if [ "$version_arg_count" -gt 0 ]; then
    echo "running timed agent version probe"
    if timeout -k 1 10 "$@"; then
      agent_version_status=0
    else
      agent_version_status=$?
    fi
    echo "timed agent version probe exit status: $agent_version_status"
    if [ "$agent_version_status" -ne 0 ]; then
      echo "agent version probe failed; skipping agent exec"
      agent_status=$agent_version_status
    fi
  fi
fi

if [ "$agent_status" -eq 0 ]; then
  arg_count="${OPENOMAN_AGENT_ARG_COUNT:-0}"
  set -- "$agent_bin"
  index=0
  while [ "$index" -lt "$arg_count" ]; do
    index_key="$(printf '%03d' "$index")"
    eval "arg_value=\${OPENOMAN_AGENT_ARG_${index_key}:-}"
    set -- "$@" "$arg_value"
    index=$((index + 1))
  done
  echo "running agent in ${OPENOMAN_AGENT_WORKDIR:-$WORKSPACE_DIR}"
  if [ "${OPENOMAN_AGENT_REDACT_API_KEY_ARGS:-0}" = "1" ]; then
    log_command_redacting_api_key "agent command: " "$@"
    log_command_argv_redacting_api_key "agent command argv:" "$@"
  else
    log_command "agent command: " "$@"
    log_command_argv "agent command argv:" "$@"
  fi
  if [ "${OPENOMAN_AGENT_REPORT_MODE:-file}" = "stdout" ]; then
    (
      cd "${OPENOMAN_AGENT_WORKDIR:-$WORKSPACE_DIR}"
      "$@"
    ) > "$OUTPUT_DIR/report.txt" || agent_status=$?
  else
    (
      cd "${OPENOMAN_AGENT_WORKDIR:-$WORKSPACE_DIR}"
      "$@"
    ) || agent_status=$?
  fi
  echo "agent exec exit status: $agent_status"
fi

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
