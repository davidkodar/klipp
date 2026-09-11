#!/usr/bin/env python3
"""Compare a Klipp viewer screenshot of the S-Log3 chart with ground truth.

    python3 tools/compare_log_chart.py <screenshot.png> <slog3_chart_expected.csv>

The chart (tools/gen_log_test_chart.py) is 1920x1080; the screenshot may be
any size with the same aspect, since the patch centres are scaled. Each patch
is the median of a 9x9 block around its centre. Prints per-patch expected vs
measured sRGB and the worst difference; exits non-zero past the tolerance
(default 3 LSB -- 8-bit viewer output plus 4:2:0 chroma and scaling).

Standard library only.
"""
import csv
import statistics
import struct
import sys
import zlib


def read_png_rgba(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, w = 8, b"", 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            w, h, depth, ctype = struct.unpack(">IIBB", body[:10])
            assert depth == 8 and ctype in (2, 6), f"need 8-bit RGB/RGBA, got {depth}/{ctype}"
            bpp = 4 if ctype == 6 else 3
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    for y in range(h):
        f = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 255
        rows.append(bytes(line))
        prev = line
    return w, h, bpp, rows


def main():
    png, expected = sys.argv[1], sys.argv[2]
    tol = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    w, h, bpp, rows = read_png_rgba(png)
    sx, sy = w / 1920.0, h / 1080.0
    worst = 0
    print(f"{'patch':<12} {'expected':>15} {'measured':>15}  diff")
    for r in csv.DictReader(open(expected)):
        cx, cy = int(int(r["x"]) * sx), int(int(r["y"]) * sy)
        chans = [[], [], []]
        for dy in range(-4, 5):
            row = rows[min(max(cy + dy, 0), h - 1)]
            for dx in range(-4, 5):
                x = min(max(cx + dx, 0), w - 1)
                for c in range(3):
                    chans[c].append(row[x * bpp + c])
        got = [int(statistics.median(ch)) for ch in chans]
        want = [int(r["srgb8_r"]), int(r["srgb8_g"]), int(r["srgb8_b"])]
        d = max(abs(g - e) for g, e in zip(got, want))
        worst = max(worst, d)
        flag = "" if d <= tol else "  <-- off"
        print(f"{r['patch']:<12} {str(want):>15} {str(got):>15}  {d}{flag}")
    print(f"worst: {worst} LSB (tolerance {tol})")
    sys.exit(0 if worst <= tol else 1)


if __name__ == "__main__":
    main()
