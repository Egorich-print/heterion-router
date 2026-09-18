#!/bin/sh
# Local-first CI gate: the exact checks `.github/workflows/ci.yml` runs.
# Usage: scripts/ci/check.sh  (from anywhere; resolves the repo root itself)
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

fail=0

echo "== exec bits =="
if git ls-files -s scripts/ | grep -v '^100755 '; then
  echo "FAIL: scripts above are not executable (git update-index --chmod=+x)"
  fail=1
else
  echo "ok"
fi

echo "== fmt =="
cargo fmt --all -- --check || fail=1

echo "== clippy =="
cargo clippy --workspace --all-targets -- -D warnings || fail=1

echo "== test =="
cargo test --workspace || fail=1

if [ "$fail" -ne 0 ]; then
  echo "check.sh: FAILURES present"
  exit 1
fi
echo "check.sh: all green"
