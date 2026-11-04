"""Generates dusty.ico and logo.svg. Run from res/: python make_icon.py [preview.png]

The mark: a cog whose bore holds an eye of two interlocking arcs.
src/ui/logo.rs builds the same shape for the app, so keep the two in step.
"""
import math
import sys
from PIL import Image, ImageDraw

SS = 1024  # supersampled canvas
TILT = math.radians(-32)
GOLD_LIGHT, GOLD_DEEP = (255, 205, 96), (190, 112, 10)


def cog(teeth, root, tip=1.0):
    """Outline with trapezoid teeth; tips narrower than their roots."""
    pts, pitch = [], 2 * math.pi / teeth
    for i in range(teeth):
        a = i * pitch
        for da, r in ((-0.31, root), (-0.17, tip), (0.17, tip), (0.31, root)):
            pts.append((r * math.cos(a + da * pitch), r * math.sin(a + da * pitch)))
    return pts


def circle(r, n=96):
    return [(r * math.cos(2 * math.pi * i / n), r * math.sin(2 * math.pi * i / n)) for i in range(n)]


def arc(side, width, bore, n=48):
    """One eyelid: a tapered band whose ends run into the cog's ring."""
    half_x, half_h = 0.74 * bore, 0.4 * bore  # where the lids cross, and how far the eye opens
    radius = (half_x**2 + half_h**2) / (2 * half_h)
    d = radius - half_h
    span = math.acos((radius**2 + d**2 - (bore + 0.04) ** 2) / (2 * d * radius))
    outer, inner = [], []
    for i in range(n + 1):
        t = i / n * 2 - 1
        th = t * span
        w = width * (1 - 0.4 * t * t) / 2
        for pts, r in ((outer, radius + w), (inner, radius - w)):
            pts.append((r * math.sin(th), side * (r * math.cos(th) - d)))
    c, s = math.cos(TILT), math.sin(TILT)
    return [(x * c - y * s, x * s + y * c) for x, y in outer + inner[::-1]]


def mark(small):
    """(solid shapes, hole shapes, shapes back on top of the holes)."""
    if small:  # 32 px and below: fewer, fatter teeth
        root, bore = 0.76, 0.54
        return [cog(12, root)], [circle(bore)], [arc(1, 0.17, bore), arc(-1, 0.17, bore)]
    root, bore = 0.82, 0.62
    return [cog(24, root)], [circle(bore)], [arc(1, 0.135, bore), arc(-1, 0.135, bore)]


def render(small):
    def px(pts):
        return [(SS / 2 + x * SS * 0.49, SS / 2 + y * SS * 0.49) for x, y in pts]

    solid, holes, top = mark(small)
    mask = Image.new("L", (SS, SS), 0)
    d = ImageDraw.Draw(mask)
    for s in solid:
        d.polygon(px(s), fill=255)
    for s in holes:
        d.polygon(px(s), fill=0)
    for s in top:
        d.polygon(px(s), fill=255)
    # Diagonal brass gradient, light from the top left.
    ramp = Image.linear_gradient("L").resize((SS, SS)).rotate(45, expand=False, fillcolor=128)
    bands = [ramp.point(lambda v, a=a, b=b: int(a + (b - a) * v / 255)) for a, b in zip(GOLD_LIGHT, GOLD_DEEP)]
    art = Image.merge("RGB", bands).convert("RGBA")
    art.putalpha(mask)
    return art


def svg(path):
    solid, holes, top = mark(False)

    def d(shapes):
        return "".join("M" + "L".join(f"{x * 49 + 50:.2f} {y * 49 + 50:.2f}" for x, y in s) + "Z" for s in shapes)

    with open(path, "w") as f:
        f.write(
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><defs>'
            '<linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop stop-color="#FFCD60"/>'
            '<stop offset="1" stop-color="#BE700A"/></linearGradient></defs>'
            f'<path fill="url(#g)" fill-rule="evenodd" d="{d(solid + holes)}"/>'
            f'<path fill="url(#g)" d="{d(top)}"/></svg>\n'
        )


big, small = render(False), render(True)
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
frames = [(small if s <= 32 else big).resize((s, s), Image.LANCZOS) for s in sizes]
frames[-1].save("dusty.ico", format="ICO", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
svg("logo.svg")

if len(sys.argv) > 1:
    sheet = Image.new("RGB", (1060, 330), (16, 25, 28))
    sheet.paste((238, 241, 240), (530, 0, 1060, 330))
    for ox in (0, 530):
        x = ox + 16
        for size, zoom in ((64, 2), (48, 2), (32, 3), (24, 4), (16, 5)):
            f = frames[sizes.index(size)]
            z = f.resize((size * zoom, size * zoom), Image.NEAREST)
            sheet.paste(z, (x, 100), z)
            x += z.width + 8
    sheet.save(sys.argv[1])
