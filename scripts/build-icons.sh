#!/usr/bin/env bash
# build-icons.sh - Generate all app icon assets from a source SVG.
#
# Usage:
#   ./scripts/build-icons.sh path/to/icon.svg
#
# Requires one of:
#   rsvg-convert  ->  brew install librsvg      (preferred, exact SVG rendering)
#   convert       ->  brew install imagemagick  (fallback)

set -euo pipefail

SVG="${1:?Usage: $0 <icon.svg>}"

if [[ ! -f "$SVG" ]]; then
  echo "error: file not found: $SVG" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ICONS_DIR="$SCRIPT_DIR/../swift-shifter/icons"
ICONSET="$ICONS_DIR/icon.iconset"


if command -v rsvg-convert &>/dev/null; then
  render() {
    local size="$1" out="$2"
    rsvg-convert -w "$size" -h "$size" "$SVG" -o "$out"
  }
elif command -v convert &>/dev/null; then
  render() {
    local size="$1" out="$2"
    convert -background none -resize "${size}x${size}" "$SVG" "$out"
  }
else
  echo "error: no SVG renderer found." >&2
  echo "  Install one of:" >&2
  echo "    brew install librsvg      # rsvg-convert (recommended)" >&2
  echo "    brew install imagemagick  # convert (fallback)" >&2
  exit 1
fi


mkdir -p "$ICONS_DIR" "$ICONSET"

echo "Rendering bundle PNGs..."
render 32  "$ICONS_DIR/32x32.png"
render 128 "$ICONS_DIR/128x128.png"
render 256 "$ICONS_DIR/128x128@2x.png"
render 256 "$ICONS_DIR/icon.png"


echo "Rendering iconset..."
render 16   "$ICONSET/icon_16x16.png"
render 32   "$ICONSET/icon_16x16@2x.png"
cp          "$ICONSET/icon_16x16@2x.png" "$ICONSET/icon_32x32.png"
render 64   "$ICONSET/icon_32x32@2x.png"
render 128  "$ICONSET/icon_128x128.png"
render 256  "$ICONSET/icon_128x128@2x.png"
cp          "$ICONSET/icon_128x128@2x.png" "$ICONSET/icon_256x256.png"
render 512  "$ICONSET/icon_256x256@2x.png"
cp          "$ICONSET/icon_256x256@2x.png" "$ICONSET/icon_512x512.png"
render 1024 "$ICONSET/icon_512x512@2x.png"


echo "Building icon.icns..."
iconutil -c icns "$ICONSET" -o "$ICONS_DIR/icon.icns"


echo "Building icon.ico..."
python3 - "$ICONS_DIR" <<'PYEOF'
import struct, subprocess, os, sys, tempfile

icons_dir = sys.argv[1]

def render_png(size):
    # Re-use the already-generated PNGs where possible, otherwise resize icon.png
    candidates = {
        16: "icon.iconset/icon_16x16.png",
        32: "icon.iconset/icon_32x32.png",
        48: None,
        256: "icon.iconset/icon_256x256.png",
    }
    src = candidates.get(size)
    if src:
        path = os.path.join(icons_dir, src)
        if os.path.exists(path):
            with open(path, "rb") as f:
                return f.read()
    # Fall back: resize icon.png with sips
    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
        tmp = f.name
    subprocess.run(
        ["sips", "-z", str(size), str(size),
         os.path.join(icons_dir, "icon.png"), "--out", tmp],
        capture_output=True, check=True,
    )
    with open(tmp, "rb") as f:
        data = f.read()
    os.unlink(tmp)
    return data

sizes = [16, 32, 48, 256]
images = [(s, render_png(s)) for s in sizes]

count = len(images)
header = struct.pack("<HHH", 0, 1, count)
offset = 6 + count * 16
entries = b""
blobs = b""
for s, png in images:
    w = 0 if s == 256 else s
    h = 0 if s == 256 else s
    entries += struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
    blobs += png

with open(os.path.join(icons_dir, "icon.ico"), "wb") as f:
    f.write(header + entries + blobs)
print(f"  icon.ico written ({len(header + entries + blobs)} bytes)")
PYEOF


echo ""
echo "Done. Icons written to swift-shifter/icons/:"
ls -lh "$ICONS_DIR"/*.png "$ICONS_DIR"/*.icns "$ICONS_DIR"/*.ico
