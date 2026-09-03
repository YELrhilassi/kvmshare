#!/usr/bin/env python3
"""Regenerate the app icons (tray + window).

Run:  python3 assets/gen_icon.py
Writes assets/tray.png (64x64) and assets/icon.png (256x256): a rounded
near-black square with a bold "K" and an emerald accent dot — the same
mark the frontend uses in its header.
"""
from PIL import Image, ImageDraw, ImageFont

BG = (15, 16, 18, 255)
FG = (233, 233, 236, 255)
ACCENT = (52, 211, 153, 255)
FONT = "/usr/share/fonts/noto/NotoSans-Bold.ttf"


def make_icon(size: int) -> Image.Image:
    s = size * 4  # supersample for crisp edges
    im = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    pad = s * 0.06
    d.rounded_rectangle((pad, pad, s - pad, s - pad), radius=s * 0.22, fill=BG)
    font = ImageFont.truetype(FONT, int(s * 0.62))
    bbox = d.textbbox((0, 0), "K", font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    d.text(
        ((s - tw) / 2 - bbox[0], (s - th) / 2 - bbox[1] - s * 0.015),
        "K", font=font, fill=FG,
    )
    r = s * 0.07
    cx, cy = s * 0.78, s * 0.78
    d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=ACCENT)
    return im.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    import os
    here = os.path.dirname(os.path.abspath(__file__))
    make_icon(64).save(os.path.join(here, "tray.png"))
    make_icon(256).save(os.path.join(here, "icon.png"))
    print("wrote tray.png + icon.png")
