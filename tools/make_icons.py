"""Cuts the app icon and the tray icon out of the TypeFlow artwork sheet.

    python tools/make_icons.py path/to/artwork.jpg

The sheet has the app icon (a rounded square on a dark page) on the left and
the tray icon (the glyphs on a black square) on the right. Writes:

- assets/icons/typeflow.ico   app icon, rounded corners transparent
- assets/icons/tray.ico       tray icon, black background made transparent
- assets/icons/tray-off.ico   the same in grey, for when hints are off

Needs Pillow and NumPy. The boxes below are measured on the 2000x1116 sheet.
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image

APP_BOX = (284, 184, 1000, 927)  # left, top, right, bottom of the rounded square
APP_RADIUS = 145
TRAY_BOX = (1419, 519, 1600, 597)  # the glyphs' ink, no margin: the icon is small enough
TRAY_SPLIT = 114  # in TRAY_BOX: "A ⇄" left of this column, "س" right of it
TRAY_SIN_ROWS = (24, 69)  # rows of TRAY_BOX that hold the "س"
TRAY_GAP = 6  # between the two rows

APP_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
TRAY_SIZES = [16, 20, 24, 28, 32, 40, 48, 64]

OUT = Path(__file__).resolve().parent.parent / "assets" / "icons"


def rounded_mask(w, h, r, k=4):
    """Antialiased rounded-rectangle coverage, k x k supersampled."""
    ys, xs = np.mgrid[0 : h * k, 0 : w * k]
    x = (xs + 0.5) / k
    y = (ys + 0.5) / k
    dx = np.maximum(np.maximum(r - x, x - (w - r)), 0)
    dy = np.maximum(np.maximum(r - y, y - (h - r)), 0)
    inside = (dx * dx + dy * dy) <= r * r
    return inside.reshape(h, k, w, k).mean(axis=(1, 3))


def app_icon(sheet):
    rgb = np.asarray(sheet.crop(APP_BOX)).astype(np.float64)
    h, w, _ = rgb.shape
    a = rounded_mask(w, h, APP_RADIUS)
    side = max(w, h)
    out = np.zeros((side, side, 4))
    oy, ox = (side - h) // 2, (side - w) // 2
    out[oy : oy + h, ox : ox + w, :3] = rgb
    out[oy : oy + h, ox : ox + w, 3] = a * 255
    return Image.fromarray(out.round().astype(np.uint8), "RGBA")


def tray_icon(sheet, grey=False):
    rgb = np.asarray(sheet.crop(TRAY_BOX)).astype(np.float64)
    # On black, each pixel is the glyph color times its coverage: take the
    # brightest channel as coverage and divide it back out.
    peak = rgb.max(axis=2)
    floor, top = 14.0, np.percentile(peak, 99.5) * 0.85
    a = np.clip((peak - floor) / (top - floor), 0, 1)
    color = np.where(peak[..., None] > 0, rgb / np.maximum(peak[..., None], 1) * 255, 0)
    color = np.clip(color, 0, 255)
    if grey:
        lum = color @ np.array([0.299, 0.587, 0.114])
        color = np.repeat((lum * 0.55 + 70)[..., None], 3, axis=2)
        a = a * 0.75
    rgba = np.dstack([color, a * 255])
    # One row is 2.3x wider than tall, so in a square it would be tiny.
    # Two rows instead: "A ⇄" on top, "س" below, under the arrows.
    top = rgba[:, :TRAY_SPLIT]
    sin = rgba[TRAY_SIN_ROWS[0] : TRAY_SIN_ROWS[1], TRAY_SPLIT:]
    th, tw = top.shape[:2]
    sh, sw = sin.shape[:2]
    side = max(tw, th + TRAY_GAP + sh)
    out = np.zeros((side, side, 4))
    ox = (side - tw) // 2
    oy = (side - (th + TRAY_GAP + sh)) // 2
    out[oy : oy + th, ox : ox + tw] = top
    sx = ox + tw - sw  # right edge lined up with the arrows' tips
    sy = oy + th + TRAY_GAP
    out[sy : sy + sh, sx : sx + sw] = np.maximum(out[sy : sy + sh, sx : sx + sw], sin)
    return Image.fromarray(out.round().astype(np.uint8), "RGBA")


def save_ico(img, name, sizes):
    big = max(sizes)
    if img.width < big:
        img = img.resize((big, big), Image.LANCZOS)
    img.save(OUT / name, format="ICO", sizes=[(s, s) for s in sizes])
    print("wrote", OUT / name)


def main():
    sheet = Image.open(sys.argv[1]).convert("RGB")
    OUT.mkdir(parents=True, exist_ok=True)
    save_ico(app_icon(sheet), "typeflow.ico", APP_SIZES)
    save_ico(tray_icon(sheet), "tray.ico", TRAY_SIZES)
    save_ico(tray_icon(sheet, grey=True), "tray-off.ico", TRAY_SIZES)


if __name__ == "__main__":
    main()
