"""Flat, hard-edged launcher icon: no gradients, glow, gloss or shadows.

Three flat colours: steel plate, HL2 orange, and one physgun-blue line.
Drawn on a 64-unit grid with straight edges and chamfered corners so it
holds up at 16 px, then rendered at each size without blur (LANCZOS on
straight edges only).
"""
import sys, os
from PIL import Image, ImageDraw

STEEL = (46, 49, 54, 255)      # plate
STEEL_EDGE = (22, 24, 27, 255)  # 1-unit outline
ORANGE = (240, 150, 42, 255)   # HL2 orange
BLUE = (82, 170, 230, 255)     # physgun line
G = 64                         # design grid

def plate(d, s):
    # chamfered square: 4-unit corner cut
    c = 4
    pts = [(c, 0), (G - c, 0), (G, c), (G, G - c), (G - c, G), (c, G), (0, G - c), (0, c)]
    d.polygon([(x * s, y * s) for x, y in pts], fill=STEEL_EDGE)
    inset = 2
    poly = []
    for x, y in pts:
        nx = inset + (x / G) * (G - 2 * inset)
        ny = inset + (y / G) * (G - 2 * inset)
        poly.append((nx * s, ny * s))
    d.polygon(poly, fill=STEEL)

def lam(d, s):
    # lambda as two thick straight strokes, drawn as polygons
    w = 9
    def stroke(p0, p1):
        (x0, y0), (x1, y1) = p0, p1
        import math
        L = math.hypot(x1 - x0, y1 - y0)
        nx, ny = -(y1 - y0) / L * w / 2, (x1 - x0) / L * w / 2
        return [((x0 + nx) * s, (y0 + ny) * s), ((x1 + nx) * s, (y1 + ny) * s),
                ((x1 - nx) * s, (y1 - ny) * s), ((x0 - nx) * s, (y0 - ny) * s)]
    long_a, long_b = (19, 11), (45, 53)
    t = 0.52
    meet = (long_a[0] + (long_b[0] - long_a[0]) * t, long_a[1] + (long_b[1] - long_a[1]) * t)
    short_a = (16, 53)
    d.polygon(stroke(long_a, long_b), fill=ORANGE)
    d.polygon(stroke(short_a, meet), fill=ORANGE)

def beam(d, s):
    # one flat blue square in the corner: the only accent besides the lambda
    d.rectangle([44 * s, 9 * s, 52 * s, 17 * s], fill=BLUE)

def render(size):
    ss = 8 if size >= 32 else 16
    S = size * ss
    unit = S / G
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    plate(d, unit)
    lam(d, unit)
    beam(d, unit)
    return img.resize((size, size), Image.LANCZOS)

def main(out_dir):
    os.makedirs(out_dir, exist_ok=True)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = {s: render(s) for s in sizes}
    frames[256].save(os.path.join(out_dir, "logo-256.png"))
    frames[128].save(os.path.join(out_dir, "logo-128.png"))
    render(512).save(os.path.join(out_dir, "logo-512.png"))
    frames[256].save(os.path.join(out_dir, "launcher.ico"), sizes=[(s, s) for s in sizes])
    print("ok", out_dir)

if __name__ == "__main__":
    main(sys.argv[1])
