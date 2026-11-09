#!/usr/bin/env bash
# Assemble the macOS .app bundle for the GPUI desktop shell.
#
# Usage: desktop/scripts/build-app.sh [--no-build]
#
#   --no-build   skip the two cargo builds and reuse the binaries already in
#                the target directories (fast path for re-assembling or for
#                iterating on the bundle metadata).
#
# The bundle is self-contained: the shell executable and the gateway sidecar
# both live in `Contents/MacOS/`, because the shell looks the gateway up next
# to its own executable (`desktop/gpui/src/sidecar.rs`, `current_exe().parent()`).
#
# The bundle is signed ad-hoc and is NOT notarized — the same posture the old
# Tauri bundle had. It runs on the machine that built it.
#
# No network access is required: both cargo builds use committed lockfiles and
# the script itself never fetches anything.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO"

APP_NAME="Heterion Router"
DIST_DIR="$REPO/desktop/gpui/dist"
APP_DIR="$DIST_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

PACKAGING_DIR="$REPO/desktop/gpui/packaging"
ICON_SRC_DIR="$PACKAGING_DIR/icons"
INFO_PLIST_SRC="$PACKAGING_DIR/Info.plist"

GPUI_MANIFEST="$REPO/desktop/gpui/Cargo.toml"
GATEWAY_MANIFEST="$REPO/crates/heterion-router-gateway/Cargo.toml"
GATEWAY_PACKAGE="heterion-router-gateway"

APP_BIN_NAME="heterion-router-desktop"
GATEWAY_BIN_NAME="heterion-router-gateway"

die() {
	echo "build-app: $*" >&2
	exit 1
}

usage() {
	cat <<'EOF'
Usage: desktop/scripts/build-app.sh [--no-build]

  --no-build   do not run cargo; reuse the binaries already built
EOF
}

NO_BUILD=0
for arg in "$@"; do
	case "$arg" in
	--no-build) NO_BUILD=1 ;;
	-h | --help)
		usage
		exit 0
		;;
	*) die "unknown argument: $arg (try --help)" ;;
	esac
done

# The effective cargo target directory. `cargo metadata` is the only source
# that knows about a `CARGO_TARGET_DIR` override or a workspace layout.
target_dir() {
	local manifest="$1" meta
	meta="$(cargo metadata --manifest-path "$manifest" --format-version 1 --no-deps --offline 2>/dev/null || true)"
	printf '%s' "$meta" |
		tr ',' '\n' |
		sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p'
}

# Build one binary and print its absolute path. Cargo's JSON message format
# is the only reliable way to learn where the artifact landed.
build_bin() {
	local manifest="$1" name="$2" log err path
	log="$(mktemp -t build-app-log)"
	err="$(mktemp -t build-app-err)"
	if ! cargo build --release --manifest-path "$manifest" --message-format=json-render-diagnostics >"$log" 2>"$err"; then
		cat "$err" >&2
		rm -f "$log" "$err"
		die "cargo build failed for $name"
	fi
	if [ -s "$err" ]; then
		cat "$err" >&2
	fi
	path="$(grep -F "\"name\":\"$name\"" "$log" 2>/dev/null |
		sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' || true)"
	rm -f "$log" "$err"
	[ -n "$path" ] || die "cargo did not report an executable for $name"
	printf '%s\n' "$path"
}

if [ "$NO_BUILD" -eq 1 ]; then
	echo "== locate binaries (no build) =="
	APP_BIN="$(target_dir "$GPUI_MANIFEST")/release/$APP_BIN_NAME"
	GATEWAY_BIN="$(target_dir "$GATEWAY_MANIFEST")/release/$GATEWAY_BIN_NAME"
	[ -f "$APP_BIN" ] ||
		die "no app binary at $APP_BIN — run without --no-build first"
	[ -f "$GATEWAY_BIN" ] ||
		die "no gateway binary at $GATEWAY_BIN — run without --no-build first"
else
	echo "== build the desktop shell =="
	APP_BIN="$(build_bin "$GPUI_MANIFEST" "$APP_BIN_NAME")"
	echo "== build the gateway sidecar =="
	GATEWAY_BIN="$(build_bin "$GATEWAY_MANIFEST" "$GATEWAY_BIN_NAME")"
fi

echo "== assemble =="
[ -d "$ICON_SRC_DIR" ] || die "missing icon set: $ICON_SRC_DIR"
[ -f "$INFO_PLIST_SRC" ] || die "missing bundle metadata: $INFO_PLIST_SRC"
[ -n "$(ls -A "$ICON_SRC_DIR")" ] || die "the icon set is empty: $ICON_SRC_DIR"

# Guard the one destructive step: only ever remove a .app inside our own dist.
case "$APP_DIR" in
"$DIST_DIR"/*.app) ;;
*) die "refusing to remove $APP_DIR" ;;
esac

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

# The shell spawns the gateway from its own directory, so both binaries go
# side by side into Contents/MacOS.
cp "$APP_BIN" "$MACOS_DIR/$APP_BIN_NAME"
cp "$GATEWAY_BIN" "$MACOS_DIR/$GATEWAY_BIN_NAME"
chmod +x "$MACOS_DIR/$APP_BIN_NAME" "$MACOS_DIR/$GATEWAY_BIN_NAME"

cp -R "$ICON_SRC_DIR/." "$RESOURCES_DIR/"
cp "$INFO_PLIST_SRC" "$CONTENTS_DIR/Info.plist"

echo "== sign (ad-hoc, not notarized) =="
# `--deep` does not reach a plain executable next to the app binary, so the
# sidecar is signed on its own first.
codesign --force --sign - "$MACOS_DIR/$GATEWAY_BIN_NAME"
codesign --force --deep --sign - "$APP_DIR"

echo "== verify =="
[ -x "$MACOS_DIR/$APP_BIN_NAME" ] || die "$APP_BIN_NAME is not executable"
[ -x "$MACOS_DIR/$GATEWAY_BIN_NAME" ] || die "$GATEWAY_BIN_NAME is not executable"
[ -f "$MACOS_DIR/$GATEWAY_BIN_NAME" ] ||
	die "the shell looks for the gateway next to itself (sidecar.rs)"

plutil -lint "$CONTENTS_DIR/Info.plist" ||
	die "$CONTENTS_DIR/Info.plist does not parse"

plist_exe="$(plutil -extract CFBundleExecutable raw -o - "$CONTENTS_DIR/Info.plist")"
[ "$plist_exe" = "$APP_BIN_NAME" ] ||
	die "Info.plist names $plist_exe but the bundle ships $APP_BIN_NAME"

plist_id="$(plutil -extract CFBundleIdentifier raw -o - "$CONTENTS_DIR/Info.plist")"
[ "$plist_id" = "com.heterion.router" ] ||
	die "Info.plist identifier is $plist_id, expected com.heterion.router"

codesign --verify --deep --strict "$APP_DIR" ||
	die "the bundle does not verify against its own signature"

echo
echo "bundle:  $APP_DIR"
echo "app:     $MACOS_DIR/$APP_BIN_NAME"
echo "gateway: $MACOS_DIR/$GATEWAY_BIN_NAME"
echo "run:     open '$APP_DIR'"
