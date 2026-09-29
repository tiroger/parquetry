# /// script
# requires-python = ">=3.9"
# dependencies = ["pillow", "numpy"]
# ///
"""Render the Parquetry app icon.

    uv run --with pillow python assets/icon/make_icon.py      # (re-execs with numpy if needed)
    uv run assets/icon/make_icon.py                           # PEP 723 metadata

Outputs (next to this file):
    icon_1024.png   1024x1024 master PNG (transparent margin + shadow, Big Sur grid)
    AppIcon.icns    built with `iconutil` from a temporary AppIcon.iconset
                    (skipped with --no-icns or when iconutil is unavailable)

Design: a macOS Big Sur-style continuous-corner squircle (824 px body on a
1024 px canvas) filled with a herringbone parquet floor in warm oak tones,
with a translucent "table header" band and faint grid lines so it reads as a
data viewer. Everything is rendered at 2x and downsampled for anti-aliasing.
"""

from __future__ import annotations

import argparse
import math
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import numpy as np
except ImportError:  # `uv run --with pillow python make_icon.py` -> add numpy and retry
    if os.environ.get("PARQUETRY_ICON_REEXEC") != "1" and shutil.which("uv"):
        os.environ["PARQUETRY_ICON_REEXEC"] = "1"
        os.execvp(
            "uv",
            ["uv", "run", "--quiet", "--with", "pillow", "--with", "numpy", "python", __file__, *sys.argv[1:]],
        )
    sys.exit("make_icon.py needs numpy: uv run --with pillow --with numpy python assets/icon/make_icon.py")

from PIL import Image, ImageChops, ImageDraw, ImageFilter

HERE = Path(__file__).resolve().parent

SS = 2                      # supersampling factor
SIZE = 1024 * SS            # working canvas
BODY = 824 * SS             # squircle body (Apple's macOS icon grid)
ORIGIN = (SIZE - BODY) // 2
SQUIRCLE_N = 5.0            # superellipse exponent; ~continuous-corner look

# Herringbone geometry (in 1x pixels): planks are RATIO times longer than wide.
RATIO = 4
# Chosen so one herringbone repeat (RATIO * sqrt(2) plank widths) equals one table
# column: the chevron spines line up with the grid.
PLANK_W = BODY / 3 / (RATIO * math.sqrt(2))
PATTERN_SHIFT = float(os.environ.get("ICON_SHIFT", "20"))
ROTATION = math.radians(45)

# Warm oak palette (sRGB): honey, amber, light oak, toasted.
OAK = np.array(
    [
        (214, 164, 104),
        (196, 138, 78),
        (226, 183, 124),
        (178, 118, 64),
    ],
    dtype=np.float32,
)
SEAM = np.array((88, 54, 28), dtype=np.float32)

HEADER_COLOR = (23, 44, 72)        # deep slate-blue band: contrasts with the wood
HEADER_ALPHA = 0.90
PILL_COLOR = (244, 232, 210)       # cream column-header chips
GRID_COLOR = (255, 247, 232)


# --------------------------------------------------------------------------- #
# Herringbone lookup
# --------------------------------------------------------------------------- #
def brick_lookup(n: int, span: int):
    """Assign every unit cell in [-span, span)^2 to a plank id.

    Axis-aligned herringbone with planks n x 1: horizontal plank H(k, m) covers
    cells (k + i + m*n, k - m*n) for i < n; vertical plank V(k, m) covers
    (k + m*n, k + 1 + j - m*n) for j < n. Translations (1, 1) and (n, -n)
    tile the plane exactly; rotating by 45 degrees gives classic herringbone.
    Returns (ids, orient) arrays indexed [x + span, y + span]; orient 0 = H, 1 = V.
    """
    size = 2 * span
    ids = np.full((size, size), -1, dtype=np.int64)
    orient = np.zeros((size, size), dtype=np.int8)
    reach = 3 * span
    next_id = 0
    for m in range(-reach // n - 2, reach // n + 3):
        for k in range(-reach, reach):
            for o in (0, 1):
                if o == 0:
                    cells = [(k + i + m * n, k - m * n) for i in range(n)]
                else:
                    cells = [(k + m * n, k + 1 + j - m * n) for j in range(n)]
                hit = False
                for x, y in cells:
                    ix, iy = x + span, y + span
                    if 0 <= ix < size and 0 <= iy < size:
                        ids[ix, iy] = next_id
                        orient[ix, iy] = o
                        hit = True
                if hit:
                    next_id += 1
    assert (ids >= 0).all(), "herringbone lookup has holes"
    return ids, orient


def value_noise(shape, scale, rng):
    """Smooth-ish noise in [0, 1] by upsampling a small random grid."""
    h, w = shape
    gh, gw = max(2, int(h / scale) + 2), max(2, int(w / scale) + 2)
    grid = rng.random((gh, gw)).astype(np.float32)
    img = Image.fromarray((grid * 255).astype(np.uint8), "L").resize((w, h), Image.BICUBIC)
    return np.asarray(img, dtype=np.float32) / 255.0


def render_wood(rng) -> np.ndarray:
    """Return an HxWx3 float32 array (0..255) of the parquet floor."""
    yy, xx = np.mgrid[0:SIZE, 0:SIZE].astype(np.float32)
    cx = cy = SIZE / 2
    # Rotate into pattern space; pattern units are plank widths.
    dx, dy = xx - cx - PATTERN_SHIFT * SS, yy - cy
    c, s = math.cos(ROTATION), math.sin(ROTATION)
    u = (dx * c + dy * s) / PLANK_W
    v = (-dx * s + dy * c) / PLANK_W

    span = int(math.ceil(SIZE * 0.75 / PLANK_W)) + 2 * RATIO
    ids, orient = brick_lookup(RATIO, span)

    iu = np.floor(u).astype(np.int64)
    iv = np.floor(v).astype(np.int64)
    fu = u - iu
    fv = v - iv
    ix, iy = iu + span, iv + span
    pid = ids[ix, iy]
    por = orient[ix, iy]

    # Per-plank tone, brightness jitter and grain phase.
    n_planks = int(ids.max()) + 1
    tone_idx = rng.integers(0, len(OAK), n_planks)
    # Avoid identical neighbours along the staircase for a livelier floor.
    for i in range(1, n_planks):
        if tone_idx[i] == tone_idx[i - 1]:
            tone_idx[i] = (tone_idx[i] + 1 + rng.integers(0, len(OAK) - 1)) % len(OAK)
    jitter = rng.normal(1.0, 0.035, n_planks).astype(np.float32)
    phase = rng.random(n_planks).astype(np.float32) * 50.0
    density = rng.uniform(5.0, 9.0, n_planks).astype(np.float32)

    base = OAK[tone_idx[pid]] * jitter[pid][..., None]

    # Along/across coordinates inside each plank (in plank widths).
    along = np.where(por == 0, u, v)
    across = np.where(por == 0, fv, fu)
    ph = phase[pid]
    dens = density[pid]
    warp = 0.18 * np.sin(along * 0.9 + ph) + 0.08 * np.sin(along * 2.3 + ph * 1.7)
    g1 = np.sin(2 * math.pi * (across * dens + warp * dens * 0.35 + ph))
    g2 = np.sin(2 * math.pi * (across * dens * 2.7 + warp * 1.3 + ph * 0.5))
    grain = 0.55 * g1 + 0.25 * g2
    grain = np.sign(grain) * np.abs(grain) ** 1.6   # thin darker streaks
    fine = value_noise((SIZE, SIZE), 3.0 * SS, rng) - 0.5
    shade = 1.0 + 0.075 * grain + 0.035 * fine
    wood = base * shade[..., None]

    # Seams: darken near a boundary with a different plank.
    def other(dx_, dy_):
        jx = np.clip(ix + dx_, 0, ids.shape[0] - 1)
        jy = np.clip(iy + dy_, 0, ids.shape[1] - 1)
        return ids[jx, jy] != pid

    seam_w = 0.048   # in plank widths (~2.3 px at 1x)
    bevel_w = 0.16
    d_left = np.where(other(-1, 0), fu, 9.0)
    d_right = np.where(other(1, 0), 1.0 - fu, 9.0)
    d_top = np.where(other(0, -1), fv, 9.0)
    d_bot = np.where(other(0, 1), 1.0 - fv, 9.0)
    dist = np.minimum(np.minimum(d_left, d_right), np.minimum(d_top, d_bot))

    seam = np.clip((seam_w - dist) / (seam_w * 0.6) + 0.5, 0.0, 1.0)
    # Soft bevel: planks darken slightly towards their edges, lighter centre.
    bevel = np.clip(1.0 - dist / bevel_w, 0.0, 1.0) ** 2
    wood = wood * (1.0 - 0.10 * bevel[..., None])
    # Light from the top: edges facing up (in screen space) catch a highlight.
    lit = np.clip(1.0 - np.minimum(d_top, d_left) / 0.09, 0.0, 1.0) * (1 - seam)
    wood = wood + 18.0 * lit[..., None]
    wood = wood * (1.0 - seam[..., None]) + SEAM * seam[..., None]
    return np.clip(wood, 0, 255)


# --------------------------------------------------------------------------- #
# Shape helpers
# --------------------------------------------------------------------------- #
def squircle_mask(size: int, body: int, origin: int) -> Image.Image:
    yy, xx = np.mgrid[0:size, 0:size].astype(np.float32)
    r = body / 2
    cx = cy = origin + r
    # Supersample edges 4x4 inside this already-2x canvas for a clean rim.
    acc = np.zeros((size, size), dtype=np.float32)
    offs = [(i + 0.5) / 4 - 0.5 for i in range(4)]
    for ox in offs:
        for oy in offs:
            nx = np.abs((xx + 0.5 + ox - cx) / r)
            ny = np.abs((yy + 0.5 + oy - cy) / r)
            acc += (nx**SQUIRCLE_N + ny**SQUIRCLE_N <= 1.0)
    return Image.fromarray((acc / 16 * 255).astype(np.uint8), "L")


def lerp(a, b, t):
    return a + (b - a) * t


def overlay_table(img: Image.Image, mask: Image.Image) -> Image.Image:
    """Translucent header band, column chips, grid lines, one selected row."""
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    top = ORIGIN
    left, right = ORIGIN, ORIGIN + BODY
    header_h = int(BODY * 0.25)
    cols = 3
    col_w = BODY / cols
    row_h = (BODY - header_h) / 5

    # Grid lines over the wood (drawn first so the header covers their tops).
    line_w = 5 * SS
    for i in range(1, cols):
        x = left + col_w * i
        d.rectangle([x - line_w / 2, top, x + line_w / 2, top + BODY], fill=GRID_COLOR + (86,))
    for j in range(1, 5):
        y = top + header_h + row_h * j
        d.rectangle([left, y - line_w / 2, right, y + line_w / 2], fill=GRID_COLOR + (86,))

    # Selected row: a soft cream wash with a crisp outline.
    sel_j = 1
    y0 = top + header_h + row_h * sel_j
    d.rectangle([left, y0, right, y0 + row_h], fill=(255, 244, 222, 70))

    # Header band.
    for yy in range(int(top), int(top + header_h)):
        t = (yy - top) / header_h
        col = tuple(int(lerp(c * 1.28, c * 0.92, t)) for c in HEADER_COLOR)
        d.line([(left, yy), (right, yy)], fill=col + (int(255 * HEADER_ALPHA),))
    # Thin highlight under the header.
    d.rectangle([left, top + header_h - 3 * SS, right, top + header_h + 3 * SS], fill=(255, 225, 170, 190))

    # Column-header chips.
    chip_h = int(header_h * 0.20)
    cy = top + header_h * 0.58
    widths = [0.52, 0.62, 0.44]
    for i in range(cols):
        cx0 = left + col_w * i + col_w * 0.17
        w = col_w * widths[i]
        d.rounded_rectangle(
            [cx0, cy - chip_h / 2, cx0 + w, cy + chip_h / 2],
            radius=chip_h / 2,
            fill=PILL_COLOR + (240,),
        )
    for i in range(1, cols):
        x = left + col_w * i
        d.rectangle([x - 2 * SS, top + header_h * 0.30, x + 2 * SS, top + header_h * 0.86], fill=(255, 255, 255, 60))

    out = Image.alpha_composite(img, layer)
    return out


def lighting(img: Image.Image) -> Image.Image:
    """Top sheen + bottom falloff + gentle vignette."""
    yy, xx = np.mgrid[0:SIZE, 0:SIZE].astype(np.float32)
    t = np.clip((yy - ORIGIN) / BODY, 0, 1)
    arr = np.asarray(img, dtype=np.float32)
    rgb = arr[..., :3]
    grad = lerp(1.06, 0.90, t**1.2)
    r = np.hypot(xx - SIZE / 2, yy - SIZE / 2) / (BODY / 2)
    vig = 1.0 - 0.10 * np.clip(r - 0.55, 0, 1) ** 1.5
    rgb = rgb * (grad * vig)[..., None]
    arr = np.concatenate([np.clip(rgb, 0, 255), arr[..., 3:]], axis=-1)
    return Image.fromarray(arr.astype(np.uint8), "RGBA")


def rim(mask: Image.Image) -> Image.Image:
    """Subtle inner stroke: light at the top, darker at the bottom."""
    inner = mask.filter(ImageFilter.MinFilter(2 * 3 * SS + 1))
    ring = ImageChops.subtract(mask, inner)
    yy = np.mgrid[0:SIZE, 0:SIZE][0].astype(np.float32)
    t = np.clip((yy - ORIGIN) / BODY, 0, 1)
    alpha = np.asarray(ring, dtype=np.float32) / 255.0
    col = np.zeros((SIZE, SIZE, 4), dtype=np.float32)
    top_col = np.array([255, 240, 215], np.float32)
    bot_col = np.array([60, 36, 18], np.float32)
    col[..., :3] = top_col * (1 - t[..., None]) + bot_col * t[..., None]
    col[..., 3] = alpha * lerp(110, 150, t)
    return Image.fromarray(col.astype(np.uint8), "RGBA")


def render(seed: int) -> Image.Image:
    rng = np.random.default_rng(seed)
    wood = render_wood(rng)
    img = Image.fromarray(wood.astype(np.uint8), "RGB").convert("RGBA")
    mask = squircle_mask(SIZE, BODY, ORIGIN)
    img = overlay_table(img, mask)
    img = lighting(img)
    img = Image.alpha_composite(img, rim(mask))
    img.putalpha(mask)

    # Drop shadow (Big Sur: soft, slightly offset downwards).
    shadow_alpha = mask.filter(ImageFilter.GaussianBlur(14 * SS)).point(lambda a: int(a * 0.42))
    shadow = Image.new("RGBA", img.size, (0, 0, 0, 0))
    shadow.putalpha(shadow_alpha)
    shadow = ImageChops.offset(shadow, 0, 10 * SS)
    ambient_alpha = mask.filter(ImageFilter.GaussianBlur(3 * SS)).point(lambda a: int(a * 0.25))
    ambient = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ambient.putalpha(ambient_alpha)
    ambient = ImageChops.offset(ambient, 0, 2 * SS)

    canvas = Image.new("RGBA", img.size, (0, 0, 0, 0))
    canvas = Image.alpha_composite(canvas, shadow)
    canvas = Image.alpha_composite(canvas, ambient)
    canvas = Image.alpha_composite(canvas, img)
    return canvas.resize((1024, 1024), Image.LANCZOS)


ICONSET = [
    (16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2),
    (256, 1), (256, 2), (512, 1), (512, 2),
]


def build_icns(master: Image.Image, out: Path) -> None:
    if not shutil.which("iconutil"):
        print("iconutil not found; skipping .icns", file=sys.stderr)
        return
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for pt, scale in ICONSET:
            px = pt * scale
            name = f"icon_{pt}x{pt}{'@2x' if scale == 2 else ''}.png"
            im = master if px == 1024 else master.resize((px, px), Image.LANCZOS)
            if px <= 64:
                # A touch of sharpening keeps seams crisp at tiny sizes.
                im = im.filter(ImageFilter.UnsharpMask(radius=0.6, percent=60, threshold=0))
            im.save(iconset / name)
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)
    print(f"wrote {out}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out-dir", type=Path, default=HERE)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--no-icns", action="store_true")
    ap.add_argument("--preview", action="store_true", help="also write a strip of small sizes")
    args = ap.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    master = render(args.seed)
    png = args.out_dir / "icon_1024.png"
    master.save(png, optimize=True)
    print(f"wrote {png}")
    if args.preview:
        sizes = [512, 256, 128, 64, 32, 16]
        strip = Image.new("RGBA", (sum(sizes) + 20 * len(sizes), 532), (236, 236, 236, 255))
        x = 10
        for s in sizes:
            im = master.resize((s, s), Image.LANCZOS)
            strip.alpha_composite(im, (x, 10))
            x += s + 20
        strip.save(args.out_dir / "preview_sizes.png")
    if not args.no_icns:
        build_icns(master, args.out_dir / "AppIcon.icns")


if __name__ == "__main__":
    main()
