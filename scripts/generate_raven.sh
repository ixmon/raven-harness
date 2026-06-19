#!/bin/bash
set -e

# Generate assets for the Raven TUI from a source PNG image.
#
# This script now supports two outputs:
# 1. Clean high-quality PNG (raven.png) - for ratatui-image (real graphics or halfblock fallback).
# 2. Low-res dithered BMP + ASCII (raven_low.bmp + raven.txt) - for classic ASCII art in the TUI.
#
# Requirements on your machine:
#   - ImageMagick (convert)
#   - jp2a (recommended for best colored ANSI output)
#     or fallback python + pillow
#
# Usage:
#   ./scripts/generate_raven.sh /path/to/your/clean_raven.png
#
# Recommended source image (generate with Grok Imagine or similar):
#   "highly detailed gothic raven perched on a gnarled wooden branch, black feathers,
#    dark atmospheric lighting, clear features and silhouette, good contrast"
#   - Crop tightly to the raven, dark background preferred.
#   - For ratatui-image: keep the source clean (no heavy dither/threshold).
#
# Output:
#   assets/raven.png            - clean version (copy your input or lightly processed)
#   assets/raven_low.bmp        - low-res for ASCII
#   assets/raven.txt / .ansi    - ASCII art versions
#
# The clean PNG is used by examples/raven_visuals.rs for modern image rendering.

if [ $# -eq 0 ]; then
    echo "Usage: $0 /path/to/raven.png"
    echo "Example: $0 ~/Downloads/gothic_raven_perch.png"
    exit 1
fi

INPUT_PNG="$1"

# Preserve a clean high-quality copy for ratatui-image usage.
# This is the recommended input for modern image rendering (no dither/threshold).
cp "$INPUT_PNG" assets/raven.png
echo "==> Saved clean source as assets/raven.png (for ratatui-image)"

# Very low resolution as requested for good TUI fit and "bitmap-like" feel.
# Example: -geometry 60x30 or even smaller like 40x22.
# Smaller geometry = chunkier, more "pixel art" / low-res BMP to ASCII look.
GEOMETRY="80x45"   # Low-res "bitmap" style for ASCII.

if [ ! -f "$INPUT_PNG" ]; then
    echo "Error: File not found: $INPUT_PNG"
    exit 1
fi

echo "==> Creating low-resolution 2-color BMP using -geometry $GEOMETRY with high contrast..."
# Note: no -negate. Dark parts of the source (raven) stay dark after threshold,
# so jp2a will use dense chars (@# etc) for the raven. Renderer maps dense->dark grays
# for a dark bird against the TUI dark background.
convert "$INPUT_PNG" \
    -geometry $GEOMETRY \
    -colorspace Gray \
    -contrast-stretch 20%x10% \
    -contrast -contrast -contrast \
    -threshold 45% \
    -dither FloydSteinberg \
    assets/raven_low.bmp

echo "==> Converting to ASCII/ANSI art..."

# Preferred: jp2a (excellent color + shading, respects the low res)
# We use --width that roughly matches the geometry width.
JP_WIDTH=$(echo $GEOMETRY | cut -d x -f 1)

if command -v jp2a >/dev/null 2>&1; then
    jp2a --colors --width=$JP_WIDTH assets/raven_low.bmp > assets/raven.ansi
    # Also produce a plain version for easy embedding (pure shading chars)
    jp2a --width=$JP_WIDTH assets/raven_low.bmp > assets/raven.txt
    echo "==> Generated assets/raven.ansi (colored) and assets/raven.txt (plain)"
else
    echo "jp2a not found. Falling back to Python + Pillow (grayscale ASCII only)."
    python3 -c '
import sys
from PIL import Image

img = Image.open("assets/raven_low.bmp").convert("L")
width, height = img.size
chars = " .:-=+*#%@"
ratio = len(chars) / 256.0

with open("assets/raven.txt", "w") as f:
    for y in range(height):
        line = ""
        for x in range(width):
            pixel = img.getpixel((x, y))
            idx = int(pixel * ratio)
            line += chars[min(idx, len(chars)-1)]
        f.write(line + "\n")
print("Generated assets/raven.txt (grayscale)")
' 2>/dev/null || echo "Please install jp2a or pillow for conversion."
fi

echo ""
echo "Done. Now:"
echo "  - Review assets/raven.txt or .ansi"
echo "  - For the TUI, you can either:"
echo "      a) Paste the content of raven.txt into the code as a multiline string (with color Spans for shading)"
echo "      b) Or load it at runtime from the file (easier to iterate)"
echo ""
echo "The art is currently hardcoded/prepended in src/tui_app.rs around the 'Gothic raven ASCII art' comment."