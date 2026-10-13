#!/usr/bin/env python3
"""Generate OCIO ACES 1.3 reference values for klipp-color's ODT parity tests.

Klipp implements the ACES 1.3 RRT + ODTs by hand in `klipp-color::aces`.
This script pushes a fixed grid of ACEScg values through OpenColorIO's
ACES 1.3 output transforms and writes the results as CSV fixtures, so
`tests/ocio_parity.rs` can verify our maths against a reference
implementation without OCIO being installed on the test machine.

Requirements: OpenColorIO >= 2.2 with Python bindings
    Arch/CachyOS:  sudo pacman -S opencolorio
    Debian/Ubuntu: apt install python3-pyopencolorio

Usage (from the workspace root):
    python tools/gen_ocio_reference.py [ODT ...]

With ODT names (the keys of `ODTS`, e.g. `rec2020_sdr`), only those
fixtures are written; the others stay as committed.

Regenerate only when deliberately re-deriving the reference — the CSVs
under crates/klipp-color/tests/fixtures/ are committed and are the
source of truth for the parity test.

IMPORTANT: the config below is pinned to an **ACES 1.3** build. OCIO
2.5 ships ACES 2.0 configs and makes one of them the default; ACES 2.0
replaced the output transform wholesale, so generating against it would
produce a wall of failures that say nothing about our correctness.
"""

import datetime
import os
import sys

try:
    import PyOpenColorIO as ocio
except ImportError:
    sys.exit(
        "PyOpenColorIO not found. Install OpenColorIO with Python bindings:\n"
        "  Arch/CachyOS:  sudo pacman -S opencolorio\n"
        "  Debian/Ubuntu: apt install python3-pyopencolorio"
    )

CONFIG = "ocio://studio-config-v2.2.0_aces-v1.3_ocio-v2.4"
SRC = "ACEScg"
OUT_DIR = os.path.join("crates", "klipp-color", "tests", "fixtures")

# Each entry maps a klipp-color function to its OCIO display + view.
#
# The P3 mapping was chosen by checking every P3-D65 view against our
# output: all of them agree on the neutral axis to ~3e-6, and
# "ACES 1.0 - SDR Cinema" is the ACES 1.0 cinema rendering our
# `acescg_to_dcip3_d65_48nit` is modelled on.
ODTS = {
    "rec709_sdr": {
        "fn": "acescg_to_rec709_sdr",
        "display": "Rec.1886 Rec.709 - Display",
        "view": "ACES 1.0 - SDR Video",
    },
    "rec2100_pq_1000": {
        "fn": "acescg_to_rec2100_pq_1000_nit",
        "display": "Rec.2100-PQ - Display",
        "view": "ACES 1.1 - HDR Video (1000 nits & Rec.2020 lim)",
    },
    "rec2100_pq_4000": {
        "fn": "acescg_to_rec2100_pq_4000_nit",
        "display": "Rec.2100-PQ - Display",
        "view": "ACES 1.1 - HDR Video (4000 nits & Rec.2020 lim)",
    },
    "dcip3_d65_48": {
        "fn": "acescg_to_dcip3_d65_48nit",
        "display": "P3-D65 - Display",
        "view": "ACES 1.0 - SDR Cinema",
    },
    "rec2020_sdr": {
        "fn": "acescg_to_rec2020_sdr",
        "display": "Rec.1886 Rec.2020 - Display",
        "view": "ACES 1.0 - SDR Video",
    },
}


def srgb_to_linear(c):
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def bt709_linear_to_acescg(rgb):
    """BT.709 linear -> ACEScg (AP1). Matches klipp_color::primaries."""
    m = [
        [0.6130974, 0.3395231, 0.0473795],
        [0.0701942, 0.9163538, 0.0134520],
        [0.0206156, 0.1095698, 0.8698146],
    ]
    return [sum(m[i][j] * rgb[j] for j in range(3)) for i in range(3)]


def fixtures():
    """The input grid. Deterministic and ordered — regenerating on an
    unchanged OCIO version must produce byte-identical CSVs.

    Note on the ColorChecker patches: they are derived from the chart's
    published *sRGB* values, not from spectral data, so they are not a
    colorimetric reference. That does not matter here — this is a
    parity test, where both sides receive the identical input and only
    the outputs are compared. Their job is to spread realistic,
    memory-colour-adjacent values across the space.
    """
    out = []

    # The six canonical patches from aces_pipeline_pin.rs, kept so the
    # two test files talk about the same colours.
    out += [
        ("mid_grey_0_18", (0.18, 0.18, 0.18)),
        ("saturated_red", (0.70, 0.05, 0.05)),
        ("saturated_green", (0.05, 0.70, 0.05)),
        ("saturated_blue", (0.05, 0.05, 0.70)),
        ("skin_tone", (0.45, 0.28, 0.20)),
        ("diffuse_white", (0.95, 0.95, 0.95)),
    ]

    # Neutral ramp: deep shadow through HDR overrange. Pins the tone
    # curve's toe, mid, shoulder and overrange clamp.
    for v in [0.0, 0.0005, 0.002, 0.01, 0.05, 0.10, 0.18, 0.25, 0.35,
              0.50, 0.75, 0.95, 1.0, 2.0, 4.0, 8.0, 16.0]:
        out.append((f"grey_{v:g}".replace(".", "p"), (v, v, v)))

    # Primaries + secondaries at three intensities — the gamut edges,
    # where the AP1 -> display matrix and the clamp behaviour matter.
    hues = {
        "red": (1.0, 0.0, 0.0), "green": (0.0, 1.0, 0.0),
        "blue": (0.0, 0.0, 1.0), "cyan": (0.0, 1.0, 1.0),
        "magenta": (1.0, 0.0, 1.0), "yellow": (1.0, 1.0, 0.0),
    }
    for name, base in hues.items():
        for lvl in [0.10, 0.50, 0.90]:
            out.append((f"{name}_{lvl:g}".replace(".", "p"),
                        tuple(c * lvl for c in base)))

    # ColorChecker 24, sRGB -> linear -> ACEScg.
    cc = [
        ("dark_skin", (115, 82, 68)), ("light_skin", (194, 150, 130)),
        ("blue_sky", (98, 122, 157)), ("foliage", (87, 108, 67)),
        ("blue_flower", (133, 128, 177)), ("bluish_green", (103, 189, 170)),
        ("orange", (214, 126, 44)), ("purplish_blue", (80, 91, 166)),
        ("moderate_red", (193, 90, 99)), ("purple", (94, 60, 108)),
        ("yellow_green", (157, 188, 64)), ("orange_yellow", (224, 163, 46)),
        ("blue", (56, 61, 150)), ("green", (70, 148, 73)),
        ("red", (175, 54, 60)), ("yellow", (231, 199, 31)),
        ("magenta", (187, 86, 149)), ("cyan", (8, 133, 161)),
        ("white_9_5", (243, 243, 242)), ("neutral_8", (200, 200, 200)),
        ("neutral_6_5", (160, 160, 160)), ("neutral_5", (122, 122, 121)),
        ("neutral_3_5", (85, 85, 85)), ("black_2", (52, 52, 52)),
    ]
    for name, rgb8 in cc:
        lin = [srgb_to_linear(c / 255.0) for c in rgb8]
        out.append((f"cc_{name}", tuple(bt709_linear_to_acescg(lin))))

    # Coarse 3D lattice over [0, 1] for broad coverage of the cube.
    steps = [0.0, 0.25, 0.5, 0.75, 1.0]
    for r in steps:
        for g in steps:
            for b in steps:
                out.append((f"lat_{r:g}_{g:g}_{b:g}".replace(".", "p"),
                            (r, g, b)))

    return out


def main():
    if not os.path.isdir(os.path.join("crates", "klipp-color")):
        sys.exit("run this from the workspace root (where Cargo.toml lives)")

    config = ocio.Config.CreateFromFile(CONFIG)
    grid = fixtures()
    os.makedirs(OUT_DIR, exist_ok=True)
    stamp = datetime.date.today().isoformat()

    wanted = sys.argv[1:] or list(ODTS)
    unknown = [k for k in wanted if k not in ODTS]
    if unknown:
        sys.exit(f"unknown ODT(s): {', '.join(unknown)}; known: {', '.join(ODTS)}")
    for key, spec in ODTS.items():
        if key not in wanted:
            continue
        xform = ocio.DisplayViewTransform(
            src=SRC, display=spec["display"], view=spec["view"]
        )
        proc = config.getProcessor(xform).getDefaultCPUProcessor()

        path = os.path.join(OUT_DIR, f"ocio_aces13_{key}.csv")
        with open(path, "w") as f:
            f.write(f"# OCIO ACES 1.3 reference for klipp_color::aces::{spec['fn']}\n")
            f.write(f"# config:   {CONFIG}\n")
            f.write(f"# src:      {SRC}\n")
            f.write(f"# display:  {spec['display']}\n")
            f.write(f"# view:     {spec['view']}\n")
            f.write(f"# ocio:     {ocio.__version__}\n")
            f.write(f"# generated: {stamp} by tools/gen_ocio_reference.py\n")
            f.write("# DO NOT EDIT BY HAND — regenerate with the script above.\n")
            f.write("name,in_r,in_g,in_b,out_r,out_g,out_b\n")
            for name, rgb in grid:
                o = proc.applyRGB(list(rgb))
                f.write(
                    f"{name},{rgb[0]:.8f},{rgb[1]:.8f},{rgb[2]:.8f},"
                    f"{o[0]:.8f},{o[1]:.8f},{o[2]:.8f}\n"
                )
        print(f"wrote {path}  ({len(grid)} values)")


if __name__ == "__main__":
    main()
