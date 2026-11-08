#!/bin/sh
# Visual-QA harness for the GPUI desktop shell.
#
# Thin wrapper: the harness lives in `gpui/tools/` next to the app it drives.
# This script only resolves the repository root, so it can be called from
# anywhere.
#
#   desktop/scripts/gpui-screenshot.sh launch
#   desktop/scripts/gpui-screenshot.sh shot login
#   desktop/scripts/gpui-screenshot.sh status
#   desktop/scripts/gpui-screenshot.sh stop
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
exec "$ROOT/desktop/gpui/tools/screenshot.sh" "$@"
