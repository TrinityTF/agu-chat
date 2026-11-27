"""Draws native/installer/icon.png (1024x1024), the app icon every platform is built from: the website's Agu Chat
mark, a white two-bubble chat glyph (lucide "messages-square") on a rounded square with the purple -> fuchsia ->
indigo diagonal gradient. make-icon.ps1 turns it into app.ico, the Mac bundle script into AppIcon.icns.

    python make-icon-png.py [out.png] [--size N] [--full]

--full fills the whole canvas edge to edge (store listings, which round the corners themselves).
"""
import math
import sys

from PIL import Image, ImageDraw

STOPS = [(0.0, (0xA8, 0x55, 0xF7)), (0.5, (0xD9, 0x46, 0xEF)), (1.0, (0x63, 0x66, 0xF1))]
SS = 4  # supersampling for smooth edges


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def gradient_at(t):
    for (t0, c0), (t1, c1) in zip(STOPS, STOPS[1:]):
        if t <= t1:
            return lerp(c0, c1, (t - t0) / (t1 - t0))
    return STOPS[-1][1]


def arc(cx, cy, r, a0, a1, steps=24):
    return [(cx + r * math.cos(math.radians(a0 + (a1 - a0) * i / steps)), cy + r * math.sin(math.radians(a0 + (a1 - a0) * i / steps))) for i in range(steps + 1)]


def glyph_paths():
    """The two strokes of lucide's messages-square in its 24-unit box (y down)."""
    # M14 9a2 2 0 0 1-2 2H6l-4 4V4a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2z
    front = [(14, 9)] + arc(12, 9, 2, 0, 90) + [(6, 11), (2, 15), (2, 4)] + arc(4, 4, 2, 180, 270) + [(12, 2)] + arc(12, 4, 2, 270, 360) + [(14, 9)]
    # M18 9h2a2 2 0 0 1 2 2v11l-4-4h-6a2 2 0 0 1-2-2v-1
    back = [(18, 9), (20, 9)] + arc(20, 11, 2, 270, 360) + [(22, 22), (18, 18), (12, 18)] + arc(12, 16, 2, 90, 180) + [(10, 15)]
    return [front, back]


def draw(size=1024, full=False):
    S = size * SS
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    box = S if full else round(S * 0.86)
    off = (S - box) // 2

    # diagonal gradient across the square, top-left to bottom-right (tailwind bg-gradient-to-br)
    grad = Image.new("RGB", (box, box))
    px = grad.load()
    for y in range(box):
        for x in range(0, box):
            px[x, y] = gradient_at((x + y) / (2 * (box - 1)))
    mask = Image.new("L", (box, box), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, box - 1, box - 1], radius=0 if full else round(box * 0.28), fill=255)
    img.paste(grad, (off, off), mask)

    # the glyph: 55% of the square, centred, stroke 2/24 of the glyph box, round caps and joins
    g = box * 0.55
    gx = off + (box - g) / 2
    unit = g / 24
    width = round(2 * unit)
    d = ImageDraw.Draw(img)
    for path in glyph_paths():
        pts = [(gx + x * unit, gx + y * unit) for x, y in path]
        # a round pen dragged along the path (discs a few pixels apart): even width, round joins and caps
        r = width / 2
        for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
            n = max(1, int(math.hypot(x1 - x0, y1 - y0) / 3))
            for i in range(n + 1):
                x, y = x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n
                d.ellipse([x - r, y - r, x + r, y + r], fill=(255, 255, 255, 255))
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    size = 1024
    if "--size" in sys.argv:
        size = int(sys.argv[sys.argv.index("--size") + 1])
        args = [a for a in args if a != str(size)]
    out = args[0] if args else __file__.replace("make-icon-png.py", "icon.png")
    draw(size, "--full" in sys.argv).save(out)
    print("wrote", out)
