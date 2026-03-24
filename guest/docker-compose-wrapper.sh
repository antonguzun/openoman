#!/bin/sh
set -eu

if [ "${1:-}" = "compose" ]; then
  shift
  exec /usr/bin/docker-compose "$@"
fi

exec /usr/bin/docker "$@"
