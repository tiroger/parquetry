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
1024 px canvas) in deep navy, holding three table columns made of chevron
parquet (warm oak planks with grain, seams and bevels), each under a cream
column-header chip. Everything is rendered at 4x and downsampled.
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

SS = 4                      # supersampling factor
SIZE = 1024 * SS            # working canvas
BODY = 824 * SS             # squircle body (Apple's macOS icon grid)
ORIGIN = (SIZE - BODY) // 2
SQUIRCLE_N = 5.0            # superellipse exponent; ~continuous-corner look

# Columns, as fractions of the body.
N_COLS = 3
COL_W = 0.195
COL_GAP = 0.058
COL_TOP = 0.305
COL_BOTTOM = 0.825
CHIP_TOP = 0.165
CHIP_BOTTOM = 0.238
# Planks: thickness and seam as fractions of the column width.
PLANK_T = 0.30
SEAM_T = 0.034

BG_TOP = (32, 44, 66)
BG_BOTTOM = (12, 18, 30)
# Warm oak palette (sRGB): light oak, honey, amber, toasted.
OAK = np.array(
    [
        (232, 190, 130),
        (218, 168, 106),
        (204, 150, 88),
        (188, 130, 72),
    ],
    dtype=np.float32,
)
SEAM = np.array((72, 43, 22), dtype=np.float32)
CHIP_TOP_COLOR = (250, 241, 224)
CHIP_BOTTOM_COLOR = (234, 219, 194)


def lerp(a, b, t):
    return a + (b - a) * t


def body_px(f: float) -> float:
    return ORIGIN + BODY * f


def column_boxes():
    w = BODY * COL_W
    g = BODY * COL_GAP
    x = ORIGIN + (BODY - (N_COLS * w + (N_COLS - 1) * g)) / 2
    for _ in range(N_COLS):
        yield x, x + w
        x += w + g


# --------------------------------------------------------------------------- #
# Chevron parquet
# --------------------------------------------------------------------------- #
def render_column(x0: float, x1: float, y0: float, y1: float, rng) -> tuple[np.ndarray, tuple[int, int, int, int]]:
    """Chevron planks filling x0..x1 x y0..y1. Returns (HxWx3 float RGB, box)."""
    bx0, by0, bx1, by1 = int(math.floor(x0)), int(math.floor(y0)), int(math.ceil(x1)), int(math.ceil(y1))
    yy, xx = np.mgrid[by0:by1, bx0:bx1].astype(np.float32)
    xx += 0.5
    yy += 0.5
    w = x1 - x0
    half = w / 2
    xc = x0 + half
    t = w * PLANK_T
    pitch = t + w * SEAM_T

    left = xx < xc
    # Each plank's top edge runs down towards the centre line at 45 degrees
    # (a V pointing down); measure vertically from it.
    rise = np.where(left, xx - x0, x1 - xx)
    v = yy - y0 - rise + pitch * 3   # keep it positive
    k = np.floor(v / pitch).astype(np.int64)
    f = v - k * pitch                # 0..pitch, plank body is 0..t
    side = (~left).astype(np.int64)

    n = int(k.max()) + 2
    tone_idx = rng.integers(0, len(OAK), (n, 2))
    for i in range(1, n):        # no identical tones stacked on the same side
        for s in (0, 1):
            if tone_idx[i, s] == tone_idx[i - 1, s]:
                tone_idx[i, s] = (tone_idx[i, s] + 1) % len(OAK)
    jitter = rng.normal(1.0, 0.03, (n, 2)).astype(np.float32)
    phase = (rng.random((n, 2)) * 50).astype(np.float32)
    density = rng.uniform(4.0, 7.0, (n, 2)).astype(np.float32)

    base = OAK[tone_idx[k, side]] * jitter[k, side][..., None]
    # The left planks face the light a little more.
    base *= np.where(left, 1.05, 0.91)[..., None]

    # Grain runs along the plank (45 degrees): stripes over the across coordinate.
    across = f / t
    along = (xx - x0) / w
    ph = phase[k, side]
    dens = density[k, side]
    warp = 0.10 * np.sin(along * 7.0 + ph) + 0.05 * np.sin(along * 17.0 + ph * 1.3)
    g1 = np.sin(2 * math.pi * (across * dens + warp * dens + ph))
    g2 = np.sin(2 * math.pi * (across * dens * 2.6 + warp * 2.0 + ph * 0.7))
    grain = 0.6 * g1 + 0.3 * g2
    grain = np.sign(grain) * np.abs(grain) ** 1.7
    shade = 1.0 + 0.07 * grain

    # Bevel towards each plank's long edges and the centre seam; highlight on top edges.
    d_edge = np.minimum(f, np.maximum(t - f, 0)) / t
    d_mid = np.abs(xx - xc) / (w * 0.08)
    bevel = np.clip(1 - np.minimum(d_edge / 0.14, d_mid), 0, 1) ** 2
    shade *= 1 - 0.12 * bevel
    lit = np.clip(1 - f / (t * 0.07), 0, 1)
    rgb = base * shade[..., None] + 22 * lit[..., None]

    seam = np.clip((f - t) / (SS * 1.0), 0, 1)
    rgb = rgb * (1 - seam[..., None]) + SEAM * seam[..., None]
    centre = np.clip(1 - np.abs(xx - xc) / (SS * 1.6), 0, 1) * 0.85
    rgb = rgb * (1 - centre[..., None]) + SEAM * centre[..., None]
    return np.clip(rgb, 0, 255), (bx0, by0, bx1, by1)


# --------------------------------------------------------------------------- #
# Shape helpers
# --------------------------------------------------------------------------- #
def squircle_mask(size: int, body: int, origin: int) -> Image.Image:
    yy, xx = np.mgrid[0:size, 0:size].astype(np.float32)
    r = body / 2
    cx = cy = origin + r
    # Supersample edges 2x2 inside this already-4x canvas for a clean rim.
    acc = np.zeros((size, size), dtype=np.float32)
    offs = [(i + 0.5) / 2 - 0.5 for i in range(2)]
    for ox in offs:
        for oy in offs:
            nx = np.abs((xx + 0.5 + ox - cx) / r)
            ny = np.abs((yy + 0.5 + oy - cy) / r)
            acc += (nx**SQUIRCLE_N + ny**SQUIRCLE_N <= 1.0)
    return Image.fromarray((acc / 4 * 255).astype(np.uint8), "L")


def vertical_gradient(top, bottom) -> Image.Image:
    t = np.clip((np.arange(SIZE, dtype=np.float32) - ORIGIN) / BODY, 0, 1)[:, None, None]
    rgb = np.array(top, np.float32) * (1 - t) + np.array(bottom, np.float32) * t
    rgb = np.broadcast_to(rgb, (SIZE, SIZE, 3))
    alpha = np.full((SIZE, SIZE, 1), 255, np.float32)
    return Image.fromarray(np.concatenate([rgb, alpha], -1).astype(np.uint8), "RGBA")


def rounded_mask(box, radius) -> Image.Image:
    m = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius=radius, fill=255)
    return m


def draw_columns(img: Image.Image, rng) -> Image.Image:
    y0, y1 = body_px(COL_TOP), body_px(COL_BOTTOM)
    chip0, chip1 = body_px(CHIP_TOP), body_px(CHIP_BOTTOM)
    shadows = Image.new("L", img.size, 0)
    sd = ImageDraw.Draw(shadows)
    boxes = list(column_boxes())
    for x0, x1 in boxes:
        r = (x1 - x0) * 0.13
        sd.rounded_rectangle([x0, y0 + 6 * SS, x1, y1 + 10 * SS], radius=r, fill=150)
        sd.rounded_rectangle([x0, chip0 + 5 * SS, x1, chip1 + 7 * SS], radius=(chip1 - chip0) / 2, fill=110)
    shadow = Image.new("RGBA", img.size, (0, 0, 0, 0))
    shadow.putalpha(shadows.filter(ImageFilter.GaussianBlur(9 * SS)))
    img = Image.alpha_composite(img, shadow)

    for x0, x1 in boxes:
        rgb, (bx0, by0, bx1, by1) = render_column(x0, x1, y0, y1, rng)
        # Inner shadow at the top so the planks sit slightly below the surface.
        h = rgb.shape[0]
        depth = np.clip(1 - np.arange(h, dtype=np.float32) / (18 * SS), 0, 1)[:, None, None] ** 2
        rgb = rgb * (1 - 0.28 * depth)
        tile = Image.fromarray(rgb.astype(np.uint8), "RGB").convert("RGBA")
        full = Image.new("RGBA", img.size, (0, 0, 0, 0))
        full.paste(tile, (bx0, by0))
        img.paste(full, (0, 0), rounded_mask([x0, y0, x1, y1], (x1 - x0) * 0.13))

        # Header chip with a soft vertical gradient.
        chip = vertical_gradient(CHIP_TOP_COLOR, CHIP_BOTTOM_COLOR)
        img.paste(chip, (0, 0), rounded_mask([x0, chip0, x1, chip1], (chip1 - chip0) / 2))
    return img


def lighting(img: Image.Image) -> Image.Image:
    """Top sheen + gentle vignette."""
    yy, xx = np.mgrid[0:SIZE, 0:SIZE].astype(np.float32)
    t = np.clip((yy - ORIGIN) / BODY, 0, 1)
    arr = np.asarray(img, dtype=np.float32)
    rgb = arr[..., :3]
    grad = lerp(1.05, 0.95, t)
    r = np.hypot(xx - SIZE / 2, yy - SIZE / 2) / (BODY / 2)
    vig = 1.0 - 0.10 * np.clip(r - 0.6, 0, 1) ** 1.5
    rgb = rgb * (grad * vig)[..., None]
    arr = np.concatenate([np.clip(rgb, 0, 255), arr[..., 3:]], axis=-1)
    return Image.fromarray(arr.astype(np.uint8), "RGBA")


def rim(mask: Image.Image) -> Image.Image:
    """Subtle inner stroke: light at the top, fading towards the bottom."""
    inner = mask.filter(ImageFilter.MinFilter(2 * 2 * SS + 1))
    ring = ImageChops.subtract(mask, inner)
    yy = np.mgrid[0:SIZE, 0:SIZE][0].astype(np.float32)
    t = np.clip((yy - ORIGIN) / BODY, 0, 1)
    alpha = np.asarray(ring, dtype=np.float32) / 255.0
    col = np.zeros((SIZE, SIZE, 4), dtype=np.float32)
    col[..., :3] = 255
    col[..., 3] = alpha * lerp(80, 20, t)
    return Image.fromarray(col.astype(np.uint8), "RGBA")


def render(seed: int) -> Image.Image:
    rng = np.random.default_rng(seed)
    mask = squircle_mask(SIZE, BODY, ORIGIN)
    img = vertical_gradient(BG_TOP, BG_BOTTOM)
    img = draw_columns(img, rng)
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
                # A touch of sharpening keeps the columns crisp at tiny sizes.
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
