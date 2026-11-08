#!/usr/bin/env bash
#
# Window probe for the Heterion Router GPUI desktop shell.
#
# Answers two questions an agent cannot answer from a build log:
#
#   1. did the app register a real window with the window server, or is it a
#      headless zombie (a process that runs but never opens anything)?
#   2. does the gateway answer on the port the shell talks to?
#
#   window_probe.sh [pid]
#
# The pid is optional: without it the probe reads the pid file the harness
# wrote, and falls back to the first GUI process carrying the app's name.
#
# Exit codes:
#   0  the app has a window
#   1  usage error
#   2  the app is not running
#   3  the app runs but no window came up
#   5  the gateway does not answer
#
# macOS only. bash + osascript + curl (plus `ps`/`awk` from the base system) —
# nothing to install.

set -euo pipefail

# shellcheck source=harness.env
# shellcheck disable=SC2034 # the paths and exit codes come from harness.env
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/harness.env"

usage() {
  sed -n '3,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# The pid to probe: an argument, the harness pid file, or the first GUI
# process that carries the app's name.
probe_pid() {
  local given="${1:-}" pid=""
  if [ -n "$given" ]; then
    printf '%s\n' "$given"
    return 0
  fi
  if pid="$(running_pid)"; then
    printf '%s\n' "$pid"
    return 0
  fi
  osa "tell application \"System Events\" to get unix id of first process whose name is \"$APP_NAME\"" |
    tr -d '\r' | head -1
}

main() {
  case "${1:-}" in
  -h | --help | help)
    usage
    return 0
    ;;
  esac

  local pid count
  pid="$(probe_pid "${1:-}" | tr -d '[:space:]')"
  if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
    printf 'app:     not running\n'
    gateway_health || true
    return "$EX_NOT_RUNNING"
  fi

  printf 'app:     running pid=%s\n' "$pid"

  if is_gui_app "$pid"; then
    printf 'gui:     yes — registered with the window server\n'
  else
    printf 'gui:     no — the window server does not see it as a GUI app\n'
  fi

  count="$(window_count "$pid")"
  if [ "$count" -ge 1 ]; then
    printf 'window:  %s open, title="%s"\n' "$count" "$(window_title "$pid" || true)"
    local geom
    if geom="$(window_geometry "$pid" || true)"; then
      printf 'geometry: %s (points, x y width height)\n' "$geom"
    fi
  else
    printf 'window:  NONE — the process is alive but opened nothing\n'
  fi

  if gateway_health; then
    gateway_ok=0
  else
    gateway_ok=1
  fi

  if [ "$count" -lt 1 ]; then
    return "$EX_NO_WINDOW"
  fi
  return "$gateway_ok"
}

main "$@"
