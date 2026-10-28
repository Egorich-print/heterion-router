#!/usr/bin/env bash
# Audit the recent agent transcripts for repetition loops.
#
# The router's guard stops a loop it sees in a response; this script finds the
# loops that already happened, by reading the sessions the runtime stored. Run
# it after a batch of agents, or when a model "says the same thing again".
#
#   ./scripts/subagent/repeat_audit.sh              # the last 24 hours
#   ./scripts/subagent/repeat_audit.sh 480          # the last 8 hours
#   ./scripts/subagent/repeat_audit.sh 0 "$HOME/.minimax/v2/sessions"
set -euo pipefail

MINUTES="${1:-1440}"
ROOT="${2:-$HOME/.minimax/v2/sessions}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 "$HERE/session_scan.py" --sessions-root "$ROOT" --since "$MINUTES" \
  | awk '/^REPEAT|^SUSPECT/ { print; getline; print "       " $0 }'
