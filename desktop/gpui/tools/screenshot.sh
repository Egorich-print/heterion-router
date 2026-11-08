#!/usr/bin/env bash
#
# Visual-QA harness for the Heterion Router GPUI desktop shell.
#
# The desktop shell is a native GPUI application, so nothing tells a reviewer
# whether it actually rendered — the only feedback would be a compiler and a
# crash log. This script closes that loop: it launches the app, waits for the
# window to come up, captures the screen and reports what it found.
#
#   launch          start the app, wait for its window, print the pid
#   shot <name>     capture the screen into tools/shots/<name>.png
#   stop            terminate the app (and the sidecar it spawned)
#   status          app state, window state, gateway health, log tail
#
# Exit codes:
#   0  success
#   1  usage error
#   2  the app is not running
#   3  no window appeared, or the capture failed
#   4  the app binary is missing (not built)
#   5  the gateway does not answer on the configured port
#
# macOS only. Nothing is installed, nothing needs the network, no credential
# ever reaches a command line.

set -euo pipefail

# shellcheck source=harness.env
# shellcheck disable=SC2034 # the paths and exit codes come from harness.env
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/harness.env"

usage() {
  sed -n '3,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# --- helpers ---------------------------------------------------------------

require_binary() {
  if [ ! -x "$APP_BIN" ]; then
    die "$EX_NOT_BUILT" "the app binary is missing: $APP_BIN
build it first:
  cargo build --manifest-path desktop/gpui/Cargo.toml"
  fi
}

# The pid of the app, the window count and the gateway state, one line each.
report_state() {
  local pid count
  if pid="$(running_pid)"; then
    count="$(window_count "$pid")"
    if [ "$count" -ge 1 ]; then
      printf 'app:     running pid=%s, %s window(s) open\n' "$pid" "$count"
    else
      printf 'app:     running pid=%s, but NO window (headless zombie)\n' "$pid"
    fi
  else
    printf 'app:     not running\n'
  fi
  gateway_health || true
}

# --- launch ----------------------------------------------------------------

cmd_launch() {
  require_binary

  local pid
  if pid="$(running_pid)"; then
    printf 'already running pid=%s\n' "$pid"
    report_state
    return 0
  fi
  rm -f "$PID_FILE"

  : >"$LOG_FILE"
  # The absolute path keeps `ps` reporting the binary we started, and nohup
  # keeps the app alive after this script exits.
  nohup "$APP_BIN" >>"$LOG_FILE" 2>&1 &
  local job=$!

  # The shell may fork the binary, so look for the real process below the job.
  local deadline=$((SECONDS + LAUNCH_TIMEOUT)) resolved=""
  while [ "$SECONDS" -lt "$deadline" ]; do
    if [ -z "$resolved" ]; then
      resolved="$(resolve_app_pid "$job" || true)"
      if [ -n "$resolved" ]; then
        printf '%s\n' "$resolved" >"$PID_FILE"
      fi
    else
      if ! kill -0 "$resolved" 2>/dev/null; then
        rm -f "$PID_FILE"
        printf 'the app exited during startup:\n' >&2
        tail -n 20 "$LOG_FILE" >&2 || true
        exit "$EX_NO_WINDOW"
      fi
      if [ "$(window_count "$resolved")" -ge 1 ]; then
        printf 'launched pid=%s window=up\n' "$resolved"
        report_state
        return 0
      fi
    fi
    sleep 0.5
  done

  if [ -z "$resolved" ]; then
    printf 'the app started but its process could not be identified\n' >&2
    tail -n 20 "$LOG_FILE" >&2 || true
    exit "$EX_NO_WINDOW"
  fi
  printf 'the app started (pid=%s) but no window appeared within %ss\n' \
    "$resolved" "$LAUNCH_TIMEOUT" >&2
  printf 'another copy of the app may already own the window (same app id).\n' >&2
  tail -n 20 "$LOG_FILE" >&2 || true
  exit "$EX_NO_WINDOW"
}

# --- shot ------------------------------------------------------------------

cmd_shot() {
  local name="${1:-}"
  [ -n "$name" ] || {
    usage >&2
    exit "$EX_USAGE"
  }
  case "$name" in
  *[!A-Za-z0-9._-]*)
    die "$EX_USAGE" "shot: the name may only hold letters, digits, dot, dash and underscore"
    ;;
  esac

  local pid count
  pid="$(running_pid || true)"
  if [ -z "$pid" ]; then
    die "$EX_NOT_RUNNING" "shot: the app is not running — run '$0 launch' first"
  fi
  count="$(window_count "$pid")"
  if [ "$count" -lt 1 ]; then
    die "$EX_NO_WINDOW" "shot: the app runs (pid=$pid) but has no window"
  fi

  mkdir -p "$SHOTS_DIR"
  local out="$SHOTS_DIR/$name.png"
  local tmpdir
  tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/gpui-shot.XXXXXX")"
  local raw="$tmpdir/screen.png"

  # Raise the app, otherwise the capture shows whatever sits in front of it.
  # The raise can be refused (another app keeps the focus), so check it.
  local front=""
  local _
  for _ in 1 2 3; do
    osa "tell application \"System Events\" to tell first process whose unix id is $pid to set frontmost to true" >/dev/null || true
    sleep 0.3
    front="$(osa "tell application \"System Events\" to get frontmost of first process whose unix id is $pid" |
      tr -d '[:space:]' || true)"
    [ "$front" = "true" ] && break
  done
  if [ "$front" != "true" ]; then
    printf 'warning: the app did not come to the front — the capture may show another window\n' >&2
  fi

  if ! screencapture -x -o "$raw"; then
    rm -rf "$tmpdir"
    die "$EX_NO_WINDOW" "shot: screencapture failed"
  fi

  # Crop the window out of the screen capture when the geometry is known.
  local mode="full screen"
  local geom x y w h logical pixel ratio
  geom="$(window_geometry "$pid" || true)"
  logical="$(desktop_size || true)"
  pixel="$(sips -g pixelWidth "$raw" 2>/dev/null | awk '/pixelWidth/ {print $2}')"
  if [ -n "$geom" ] && [ -n "$logical" ] && [ -n "$pixel" ]; then
    read -r x y w h <<<"$geom"
    read -r logical_w _ <<<"$logical"
    if [ "$logical_w" -gt 0 ] && [ "$pixel" -gt 0 ]; then
      ratio=$((pixel / logical_w))
      [ "$ratio" -ge 1 ] || ratio=1
      if sips -c $((h * ratio)) $((w * ratio)) --cropOffset $((y * ratio)) $((x * ratio)) \
        "$raw" --out "$out" >/dev/null 2>&1; then
        mode="window crop ${x},${y} ${w}x${h} (${ratio}x)"
      fi
    fi
  fi
  if [ "$mode" = "full screen" ]; then
    cp "$raw" "$out"
  fi
  rm -rf "$tmpdir"

  local title
  title="$(window_title "$pid" || true)"
  printf 'shot: %s [%s] window="%s"\n' "$out" "$mode" "$title"
}

# --- stop ------------------------------------------------------------------

cmd_stop() {
  local pid
  pid="$(running_pid || true)"
  if [ -z "$pid" ]; then
    rm -f "$PID_FILE"
    printf 'stop: the app is not running\n'
    return 0
  fi

  # The sidecar the app spawned is a child of it — kill that child and
  # nothing else. No pattern matching over the process table.
  local child comm
  for child in $(descendants_of "$pid"); do
    comm="$(ps -p "$child" -o comm= 2>/dev/null || true)"
    case "$comm" in
    *heterion-router-gateway)
      kill -TERM "$child" 2>/dev/null || true
      ;;
    esac
  done

  kill -TERM "$pid" 2>/dev/null || true
  local waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.5
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    kill -KILL "$pid" 2>/dev/null || true
    printf 'stop: pid=%s needed SIGKILL\n' "$pid"
  else
    printf 'stop: pid=%s terminated\n' "$pid"
  fi
  rm -f "$PID_FILE"
}

# --- status ----------------------------------------------------------------

cmd_status() {
  local pid count
  if pid="$(running_pid)"; then
    count="$(window_count "$pid")"
    if [ "$count" -ge 1 ]; then
      printf 'app:     running pid=%s\n' "$pid"
      printf 'window:  %s open, title="%s"\n' "$count" "$(window_title "$pid" || true)"
      local geom
      if geom="$(window_geometry "$pid" || true)"; then
        printf 'geometry: %s (points, x y width height)\n' "$geom"
      fi
    else
      printf 'app:     running pid=%s\n' "$pid"
      printf 'window:  NONE — the process is alive but no window came up\n'
    fi
  else
    printf 'app:     not running\n'
  fi

  if gateway_health; then
    :
  else
    printf 'gateway: unreachable\n'
  fi
  operator_key_status

  printf 'log:     %s\n' "$LOG_FILE"
  if [ -f "$LOG_FILE" ]; then
    local tail_out
    tail_out="$(tail -n 15 "$LOG_FILE")"
    if [ -n "$tail_out" ]; then
      printf '%s\n' "$tail_out" | sed 's/^/         /'
    else
      printf '         (empty — the app logged nothing)\n'
    fi
  else
    printf '         (no log yet)\n'
  fi

  pid="$(running_pid || true)"
  if [ -z "$pid" ]; then
    return "$EX_NOT_RUNNING"
  fi
  if [ "$(window_count "$pid")" -lt 1 ]; then
    return "$EX_NO_WINDOW"
  fi
  return "$EX_OK"
}

# --- main ------------------------------------------------------------------

main() {
  local command="${1:-}"
  [ -n "$command" ] || {
    usage >&2
    exit "$EX_USAGE"
  }
  shift
  case "$command" in
  launch) cmd_launch "$@" ;;
  shot) cmd_shot "$@" ;;
  stop) cmd_stop "$@" ;;
  status) cmd_status "$@" ;;
  -h | --help | help)
    usage
    ;;
  *)
    printf 'unknown command: %s\n\n' "$command" >&2
    usage >&2
    exit "$EX_USAGE"
    ;;
  esac
}

main "$@"
