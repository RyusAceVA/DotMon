#!/usr/bin/env bash
# Regenerates the PNG icons from dotmon.svg (needs rsvg-convert, from librsvg).
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p png
for size in 16 24 32 48 64 128 256 512; do
    rsvg-convert -w "$size" -h "$size" dotmon.svg -o "png/dotmon-$size.png"
done
echo "Icônes régénérées dans $(pwd)/png"
