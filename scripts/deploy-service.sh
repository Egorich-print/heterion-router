#!/bin/sh
# Deploy the gateway as a launchd service.
#
# The service runs from a copy on the system disk (~/.local/bin), not from
# target/release on the external volume: macOS runs a trust/signature
# assessment for a freshly written binary on a mounted volume, and that
# assessment can hang the exec — the process then sits in dyld with 0% CPU
# and never binds its port. A copy on the system disk starts every time and
# also keeps the service alive when the volume is unmounted.
#
# Usage: scripts/deploy-service.sh [--no-restart]
set -eu

REPO="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${HETERION_SERVICE_BIN:-$HOME/.local/bin/heterion-router-gateway}"
PLIST="$HOME/Library/LaunchAgents/com.heterion.router.plist"
JOB="com.heterion.router"
DOMAIN="gui/$(id -u)"

echo "== build =="
cd "$REPO"
cargo build --release

echo "== install to $DEST =="
mkdir -p "$(dirname "$DEST")"
# Copy through a temporary name: replacing a running executable in place
# leaves the old inode mapped, and the new file is what gets signed.
cp "$REPO/target/release/heterion-router-gateway" "$DEST.tmp"
chmod +x "$DEST.tmp"
codesign --force --sign - "$DEST.tmp" >/dev/null 2>&1 || true
mv "$DEST.tmp" "$DEST"

if [ "${1:-}" = "--no-restart" ]; then
  echo "== installed, not restarted =="
  exit 0
fi

echo "== point the job at $DEST =="
if [ -f "$PLIST" ]; then
  cp "$PLIST" "$PLIST.bak-$(date +%Y-%m-%d-%H%M%S)"
  /usr/libexec/PlistBuddy -c "Set :ProgramArguments:0 $DEST" "$PLIST"
fi

echo "== reload the job =="
# launchd keeps a cached copy of the job definition, so kickstart alone
# would keep running the old binary: the job has to be re-registered.
launchctl bootout "$DOMAIN/$JOB" 2>/dev/null || true
sleep 1
launchctl bootstrap "$DOMAIN" "$PLIST"

echo "== wait for the port =="
for _ in $(seq 1 30); do
  if curl -s -m 3 http://127.0.0.1:20128/healthz >/dev/null 2>&1; then
    curl -s http://127.0.0.1:20128/healthz
    echo
    echo "deployed: $DEST"
    exit 0
  fi
  sleep 1
done

echo "the service did not answer on 20128" >&2
exit 1
