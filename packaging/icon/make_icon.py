#!/usr/bin/env python3
"""Erzeugt das App-Symbol: ein Ordner, wie mit Microsoft Paint hingekritzelt.

Kleine Leinwand ohne Kantenglättung, zittrige doppelte Umrisslinien, Füllung mit dem
„Farbeimer“ (inklusive kleiner Lücken) – danach pixelgenau vergrößert. Feste Zufallszahl,
damit jeder Lauf dasselbe Bild ergibt.

    python3 packaging/icon/make_icon.py
"""
import random
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
BASE = 64  # Leinwand in „Paint-Pixeln“
BLACK = (0, 0, 0, 255)
WHITE = (255, 255, 255, 255)
YELLOW = (255, 242, 0, 255)  # Paint-Gelb
GOLD = (255, 201, 14, 255)  # Paint-Gold (Rückseite)


def wobble(points, rnd, step=5, amp=1.3):
    """Polygonzug in kurze Stücke teilen und jedes Stück leicht verwackeln (Hand mit Maus)."""
    out = []
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        n = max(1, int(max(abs(x1 - x0), abs(y1 - y0)) / step))
        for i in range(n):
            t = i / n
            out.append((x0 + (x1 - x0) * t + rnd.uniform(-amp, amp), y0 + (y1 - y0) * t + rnd.uniform(-amp, amp)))
    out.append((points[-1][0] + rnd.uniform(-amp, amp), points[-1][1] + rnd.uniform(-amp, amp)))
    return out


def scribble(draw, points, rnd, width=2, passes=2):
    """Mehrfach nachgezogene, nicht ganz deckungsgleiche Linie mit Überschwingern an den Enden."""
    for p in range(passes):
        pts = wobble(points, rnd, amp=0.9 + 0.5 * p)
        # Überschwinger: Anfang ein Stück vor, Ende ein Stück über das Ziel hinaus
        (ax, ay), (bx, by) = pts[0], pts[1]
        pts.insert(0, (ax - (bx - ax) * 0.4, ay - (by - ay) * 0.4))
        draw.line(pts, fill=BLACK, width=width, joint=None)


def folder(seed=7):
    rnd = random.Random(seed)
    im = Image.new("RGBA", (BASE, BASE), WHITE)
    d = ImageDraw.Draw(im)
    # Rückseite mit Reiter
    back = [(9, 22), (9, 13), (24, 13), (28, 18), (54, 18), (54, 50), (9, 50), (9, 22)]
    # Vorderklappe, leicht schräg (Perspektive wie in Kinderzeichnungen)
    front = [(9, 27), (57, 25), (52, 51), (8, 51), (9, 27)]
    scribble(d, back, rnd)
    ImageDraw.floodfill(im, (16, 16), GOLD, thresh=10)
    ImageDraw.floodfill(im, (50, 21), GOLD, thresh=10)
    scribble(d, front, rnd)
    ImageDraw.floodfill(im, (30, 40), YELLOW, thresh=10)
    # Schneller Glanz oben links und hingekritzelte Schraffur unten rechts
    d.line([(13, 34), (17, 30)], fill=WHITE, width=2)
    d.line([(19, 33), (21, 31)], fill=WHITE, width=2)
    for x in range(36, 50, 4):
        d.line([(x, 47), (x + 5, 40)], fill=GOLD, width=1)
    return im


def render(size, round_mask=False):
    art = folder().resize((size, size), Image.NEAREST)
    if not round_mask:
        return art
    mask = Image.new("L", (BASE, BASE), 0)
    ImageDraw.Draw(mask).ellipse((0, 0, BASE - 1, BASE - 1), fill=255)
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(art, (0, 0), mask.resize((size, size), Image.NEAREST))
    return out


def main():
    res = ROOT / "apps/mobile/app/android/app/src/main/res"
    for folder_name, px in {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}.items():
        render(px).save(res / f"mipmap-{folder_name}/ic_launcher.png", optimize=True)
        render(px, round_mask=True).save(res / f"mipmap-{folder_name}/ic_launcher_round.png", optimize=True)
    icons = ROOT / "apps/desktop/src-tauri/icons"
    for name, px in {"32x32.png": 32, "128x128.png": 128, "128x128@2x.png": 256, "icon.png": 512}.items():
        render(px).save(icons / name, optimize=True)
    # Vorschau neben dem Skript (für README/Store)
    render(512).save(Path(__file__).with_name("icon-512.png"), optimize=True)


if __name__ == "__main__":
    main()
