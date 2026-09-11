#!/usr/bin/env python3
"""Generate an S-Log3 / S-Gamut3.Cine test chart with known ground truth.

Writes a 10-bit HEVC (NVDEC -> P010 path) and an 8-bit HEVC (NVDEC -> NV12
path), both lossless, plus a ProRes 422 HQ (software decode -- the Atomos /
Blackmagic recorder case); limited range, BT.709 matrix, the way Sony
cameras record. The encoding uses colour-science, independent of Klipp's own
curves and matrices, so a live check against the file is a real check.

Also writes `<out>/slog3_chart_expected.csv`: for each patch, its centre in
the frame, the BT.709 scene-linear value it encodes, and the 8-bit sRGB
value Klipp's viewer should show for it at an identity grade (scene-linear
x 0.723171, Narkowicz filmic, sRGB encode -- the "converted automatically"
look).

Klipp cannot tell from these files that they are S-Log3 (no camera
metadata), which is deliberate: it exercises declaring the format with
`klipp_set_log_format`.

Needs numpy + colour-science:
    python -m venv v && v/bin/pip install colour-science
    v/bin/python tools/gen_log_test_chart.py <out-dir>
"""
import csv
import subprocess
import sys
from pathlib import Path

import colour
import numpy as np

W, H = 1920, 1080
FPS = 25
SECONDS = 5
LOG_DISPLAY_EXPOSURE = 0.723171

# Row 1: neutrals, one stop apart around 18% grey.
NEUTRAL_STOPS = list(range(-6, 6))
# Row 2: BT.709 scene-linear colours.
COLOURS = [
    ("red", (0.30, 0.05, 0.05)),
    ("green", (0.05, 0.30, 0.05)),
    ("blue", (0.05, 0.05, 0.30)),
    ("yellow", (0.30, 0.30, 0.05)),
    ("cyan", (0.05, 0.30, 0.30)),
    ("magenta", (0.30, 0.05, 0.30)),
    ("skin", (0.40, 0.25, 0.18)),
    ("hot_orange", (1.50, 0.60, 0.10)),
]
# Row 3: a continuous exposure ramp, into S-Log3 super-white.
RAMP_STOPS = (-8.0, 7.5)

ROWS = [(120, 360), (420, 660), (720, 960)]


def filmic(x):
    a, b, c, d, e = 2.51, 0.03, 2.43, 0.59, 0.14
    return np.clip((x * (a * x + b)) / (x * (c * x + d) + e), 0.0, 1.0)


def expected_srgb8(scene_709):
    display = filmic(np.maximum(np.asarray(scene_709) * LOG_DISPLAY_EXPOSURE, 0.0))
    return np.round(colour.models.eotf_inverse_sRGB(display) * 255.0).astype(int)


def build_scene():
    """BT.709 scene-linear image, plus the patch list for the CSV."""
    img = np.full((H, W, 3), 0.18)
    patches = []
    y0, y1 = ROWS[0]
    pw = W // len(NEUTRAL_STOPS)
    for i, k in enumerate(NEUTRAL_STOPS):
        v = 0.18 * 2.0**k
        img[y0:y1, i * pw:(i + 1) * pw] = v
        patches.append((f"grey_{k:+d}", i * pw + pw // 2, (y0 + y1) // 2, (v, v, v)))
    y0, y1 = ROWS[1]
    pw = W // len(COLOURS)
    for i, (name, rgb) in enumerate(COLOURS):
        img[y0:y1, i * pw:(i + 1) * pw] = rgb
        patches.append((name, i * pw + pw // 2, (y0 + y1) // 2, rgb))
    y0, y1 = ROWS[2]
    stops = np.linspace(RAMP_STOPS[0], RAMP_STOPS[1], W)
    img[y0:y1, :] = (0.18 * 2.0**stops)[None, :, None]
    return img, patches


def scene_to_slog3_codes(scene_709):
    """BT.709 scene-linear -> S-Gamut3.Cine -> S-Log3 10-bit code values."""
    sgamut = colour.RGB_to_RGB(
        scene_709,
        colour.RGB_COLOURSPACES["ITU-R BT.709"],
        colour.RGB_COLOURSPACES["S-Gamut3.Cine"],
        chromatic_adaptation_transform=None,
    )
    cv = colour.models.log_encoding_SLog3(np.maximum(sgamut, 0.0), out_normalised_code_value=True)
    return cv * 1023.0


def codes_to_yuv420(codes10, bits, chroma_rows=2):
    """R'G'B' code values -> limited-range BT.709 Y'CbCr planes: 4:2:0, or
    4:2:2 with `chroma_rows=1`."""
    e = (codes10 - 64.0) / 876.0  # legal-normalised R'G'B'
    r, g, b = e[..., 0], e[..., 1], e[..., 2]
    y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    cb = (b - y) / 1.8556
    cr = (r - y) / 1.5748
    scale = 2 ** (bits - 8)
    ymax = (1 << bits) - 1
    Y = np.clip(np.round((16 + 219 * y) * scale), 0, ymax)
    # Chroma: average 2x2 (or 2x1) blocks; patch edges are on even pixels.
    r_ = chroma_rows
    cb2 = cb.reshape(H // r_, r_, W // 2, 2).mean(axis=(1, 3))
    cr2 = cr.reshape(H // r_, r_, W // 2, 2).mean(axis=(1, 3))
    U = np.clip(np.round((128 + 224 * cb2) * scale), 0, ymax)
    V = np.clip(np.round((128 + 224 * cr2) * scale), 0, ymax)
    dtype = np.uint16 if bits > 8 else np.uint8
    return [p.astype(dtype) for p in (Y, U, V)]


def encode(planes, bits, path, prores=False):
    # One frame on disk, looped by ffmpeg (it cannot loop stdin).
    frame = path.with_suffix(".yuv")
    frame.write_bytes(b"".join(p.tobytes() for p in planes))
    pix = "yuv422p10le" if prores else ("yuv420p10le" if bits == 10 else "yuv420p")
    codec = (["-c:v", "prores_ks", "-profile:v", "3", "-vendor", "apl0"] if prores
             else ["-c:v", "libx265", "-x265-params", "lossless=1:log-level=error", "-tag:v", "hvc1"])
    # The same tags on input and output. Tagging only the output makes
    # ffmpeg insert a silent range/matrix conversion (super-whites clipped
    # to 940, 8-bit grey 105 -> 104), which would wreck the ground truth.
    tags = ["-color_range", "tv", "-colorspace", "bt709",
            "-color_primaries", "bt709", "-color_trc", "bt709"]
    cmd = [
        "ffmpeg", "-nostdin", "-y", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", pix, "-s", f"{W}x{H}", "-r", str(FPS), *tags,
        "-stream_loop", str(FPS * SECONDS - 1), "-i", str(frame),
        *codec, "-pix_fmt", pix, *tags,
        str(path),
    ]
    try:
        subprocess.run(cmd, check=True)
    finally:
        frame.unlink(missing_ok=True)


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    out.mkdir(parents=True, exist_ok=True)
    scene, patches = build_scene()
    codes = scene_to_slog3_codes(scene)
    print(f"S-Log3 code range: {codes.min():.1f} .. {codes.max():.1f}")
    for bits, name in [(10, "slog3_chart_10bit.mp4"), (8, "slog3_chart_8bit.mp4")]:
        encode(codes_to_yuv420(codes, bits), bits, out / name)
        print(f"wrote {out / name}")
    encode(codes_to_yuv420(codes, 10, chroma_rows=1), 10, out / "slog3_chart_prores.mov", prores=True)
    print(f"wrote {out / 'slog3_chart_prores.mov'}")
    with open(out / "slog3_chart_expected.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["patch", "x", "y", "scene_r", "scene_g", "scene_b",
                    "slog3_code_g", "srgb8_r", "srgb8_g", "srgb8_b"])
        for name, x, y, rgb in patches:
            e = expected_srgb8(rgb)
            w.writerow([name, x, y, *rgb, f"{codes[y, x, 1]:.1f}", *e])
    print(f"wrote {out / 'slog3_chart_expected.csv'}")


if __name__ == "__main__":
    main()
