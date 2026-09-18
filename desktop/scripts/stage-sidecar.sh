#!/bin/sh
# Stage the gateway binary as a Tauri sidecar.
#
# Copies `target/release/heterion-router-gateway` to
# `src-tauri/binaries/heterion-router-gateway-<target-triple>`, where Tauri's
# `externalBin` picks it up for bundling (and for `tauri dev`). Location
# independent: resolves the repo root from this script's own path.
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
test -n "$TRIPLE" || { echo "stage-sidecar: cannot determine host triple" >&2; exit 1; }

cargo build --release -p heterion-router-gateway --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$ROOT/desktop/src-tauri/binaries"
cp "$ROOT/target/release/heterion-router-gateway" \
  "$ROOT/desktop/src-tauri/binaries/heterion-router-gateway-$TRIPLE"
echo "stage-sidecar: staged heterion-router-gateway-$TRIPLE"
