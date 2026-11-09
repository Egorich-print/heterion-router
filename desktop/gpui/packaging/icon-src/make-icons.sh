#!/usr/bin/env bash
# Render the Heterion Router icon set from the SVG masters.
#
# Usage: desktop/gpui/packaging/icon-src/make-icons.sh
#
# Sources (edit these, never the PNGs):
#   icon.svg       macOS master — squircle plate + mark, 1024x1024 canvas
#   icon-mark.svg  mark only on a transparent background (Windows assets)
#
# Outputs (desktop/gpui/packaging/icons/, the set the .app bundle ships):
#   icon.png, 32x32.png, 128x128.png, 128x128@2x.png, icon.icns, icon.ico
#   Square<nn>x<nn>Logo.png, StoreLogo.png
#
# Requires: rsvg-convert, iconutil (macOS), ImageMagick.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/../icons"
MAC_MASTER="$HERE/icon.svg"
MARK_MASTER="$HERE/icon-mark.svg"

die() {
	echo "make-icons: $*" >&2
	exit 1
}

need() {
	command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"
}

need rsvg-convert
need iconutil
need magick

[ -f "$MAC_MASTER" ] || die "missing master: $MAC_MASTER"
[ -f "$MARK_MASTER" ] || die "missing master: $MARK_MASTER"

mkdir -p "$OUT"

# render <svg> <size> <destination>
render() {
	rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
}

echo "== macOS assets (squircle plate) =="
render "$MAC_MASTER" 512 "$OUT/icon.png"
render "$MAC_MASTER" 32 "$OUT/32x32.png"
render "$MAC_MASTER" 128 "$OUT/128x128.png"
render "$MAC_MASTER" 256 "$OUT/128x128@2x.png"

echo "== macOS icns =="
# iconutil only accepts an .iconset directory with Apple's exact file names.
ICONSET_ROOT="$(mktemp -d)"
trap 'rm -rf "$ICONSET_ROOT"' EXIT
ICONSET="$ICONSET_ROOT/icon.iconset"
mkdir -p "$ICONSET"
render "$MAC_MASTER" 16 "$ICONSET/icon_16x16.png"
render "$MAC_MASTER" 32 "$ICONSET/icon_16x16@2x.png"
render "$MAC_MASTER" 32 "$ICONSET/icon_32x32.png"
render "$MAC_MASTER" 64 "$ICONSET/icon_32x32@2x.png"
render "$MAC_MASTER" 128 "$ICONSET/icon_128x128.png"
render "$MAC_MASTER" 256 "$ICONSET/icon_128x128@2x.png"
render "$MAC_MASTER" 256 "$ICONSET/icon_256x256.png"
render "$MAC_MASTER" 512 "$ICONSET/icon_256x256@2x.png"
render "$MAC_MASTER" 512 "$ICONSET/icon_512x512.png"
render "$MAC_MASTER" 1024 "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$OUT/icon.icns"

echo "== ico (Windows, multi-resolution) =="
magick "$OUT/icon.png" \
	-define icon:auto-resize=256,128,64,48,32,16 \
	"$OUT/icon.ico"

echo "== Windows assets (mark only, transparent) =="
for size in 30 44 71 89 107 142 150 284 310; do
	render "$MARK_MASTER" "$size" "$OUT/Square${size}x${size}Logo.png"
done
render "$MARK_MASTER" 50 "$OUT/StoreLogo.png"

echo
echo "icon set: $OUT"
ls -1 "$OUT"
