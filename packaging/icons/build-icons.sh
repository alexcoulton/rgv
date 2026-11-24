#!/usr/bin/env bash
# Regenerates the raster app icons from the SVG masters. Runs on macOS with Homebrew's librsvg
# and ImageMagick: rsvg-convert renders (keeping transparency), iconutil builds the .icns and
# ImageMagick the .ico.
#
#   rgv.svg        master, full bleed: Windows, Linux and the runtime window icon
#   rgv-macos.svg  the same mark on Apple's icon grid
#
# Outputs (committed): rgv.icns, rgv.ico, rgv-256.png.
set -euo pipefail
cd "$(dirname "$0")"
work="$(mktemp -d)"
trap 'rm -r "$work"' EXIT

render() { # svg size output
    rsvg-convert --width "$2" --height "$2" --keep-aspect-ratio --output "$3" "$1"
}

iconset="$work/rgv.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    render rgv-macos.svg "$size" "$iconset/icon_${size}x${size}.png"
    render rgv-macos.svg "$((size * 2))" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns -o rgv.icns "$iconset"

for size in 16 24 32 48 64 128 256; do
    render rgv.svg "$size" "$work/ico-$size.png"
done
magick "$work"/ico-{16,24,32,48,64,128,256}.png rgv.ico
render rgv.svg 256 rgv-256.png
