#!/usr/bin/env python3
"""Generate Android launcher icons from the macOS Van Gogh pixel portrait.

Run from the repository root with Pillow installed:
    python3 Scripts/generate_android_icons.py
"""

from collections import deque
from pathlib import Path

from PIL import Image


ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets/Assets.xcassets/AppIcon.appiconset/icon_512x512.png"
RES = ROOT / "app/android/app/src/main/res"
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}


def portrait_without_background(source: Image.Image) -> Image.Image:
    """Clear the nearly white background connected to the source image edges."""
    portrait = source.convert("RGBA")
    pixels = portrait.load()
    width, height = portrait.size
    edge = deque()
    for x in range(width):
        edge.extend(((x, 0), (x, height - 1)))
    for y in range(height):
        edge.extend(((0, y), (width - 1, y)))

    background = set()
    while edge:
        x, y = edge.popleft()
        if (x, y) in background or min(pixels[x, y][:3]) < 240:
            continue
        background.add((x, y))
        if x > 0:
            edge.append((x - 1, y))
        if x + 1 < width:
            edge.append((x + 1, y))
        if y > 0:
            edge.append((x, y - 1))
        if y + 1 < height:
            edge.append((x, y + 1))

    for x, y in background:
        pixels[x, y] = (0, 0, 0, 0)
    return portrait


def main() -> None:
    source = Image.open(SOURCE).convert("RGBA")
    foreground = portrait_without_background(source)
    for density, scale in DENSITIES.items():
        target = RES / f"mipmap-{density}"
        legacy_size = round(48 * scale)
        source.resize((legacy_size, legacy_size), Image.Resampling.NEAREST).save(
            target / "ic_launcher.png"
        )

        # Adaptive icons have a 108dp canvas and a centered 66dp safe circle.
        # The portrait fills that circle without losing its hair or beard.
        canvas_size = round(108 * scale)
        artwork_size = round(canvas_size * 0.68)
        artwork = foreground.resize(
            (artwork_size, artwork_size), Image.Resampling.NEAREST
        )
        canvas = Image.new("RGBA", (canvas_size, canvas_size))
        offset = (canvas_size - artwork_size) // 2
        canvas.alpha_composite(artwork, (offset, offset))
        canvas.save(target / "ic_launcher_foreground.png")


if __name__ == "__main__":
    main()
