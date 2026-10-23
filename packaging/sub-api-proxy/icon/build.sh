#!/bin/sh
# Regenerates every committed raster icon from the SVG sources in this directory.
# Needs only macOS tools (sips, iconutil).
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
macos=$here/../macos
ui=$here/../../../ui/sap/public
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# render SVG SIZE OUT: sips rasterizes an SVG at its declared size, so render large and downscale.
render() {
    sed "s/<svg /<svg width=\"1152\" height=\"1152\" /" "$1" > "$work/src.svg"
    sips -s format png "$work/src.svg" --out "$work/big.png" >/dev/null
    sips -z "$2" "$2" "$work/big.png" --out "$3" >/dev/null
}

render "$here/menubar.svg" 18 "$macos/MenuBarIconTemplate.png"
render "$here/menubar.svg" 36 "$macos/MenuBarIconTemplate@2x.png"

iconset=$work/AppIcon.iconset
mkdir "$iconset"
for size in 16 32 128 256 512; do
    render "$here/app.svg" "$size" "$iconset/icon_${size}x${size}.png"
    render "$here/app.svg" "$((size * 2))" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$macos/AppIcon.icns"

cp "$here/favicon.svg" "$ui/favicon.svg"
render "$here/favicon.svg" 180 "$ui/apple-touch-icon.png"
printf 'Icons written to %s and %s\n' "$macos" "$ui"
