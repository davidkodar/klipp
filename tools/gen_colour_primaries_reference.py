#!/usr/bin/env python3
"""Generate every source-primaries -> linear BT.709 matrix Klipp uses,
from colour-science, as both a committed CSV fixture and the Rust table in
klipp-color's camera_log.rs.

The previous table was hand-typed and wrong for most camera gamuts — by up
to 0.57 per element for RED Wide Gamut, 0.36 for S-Gamut3. colour-science
derives each matrix from the published chromaticities (Bradford adaptation
where white points differ); for S-Gamut3, S-Gamut3.Cine and ARRI Wide
Gamut 3 it was cross-checked against the matrices inside OCIO's manufacturer
camera transforms and agreed to ~5e-3 (OCIO uses the manufacturers' rounded
published matrices).

Usage (from the workspace root, colour-science installed):
    python tools/gen_colour_primaries_reference.py > /tmp/table.rs
Writes the fixture; prints the Rust match arms to stdout.
"""

import datetime
import os
import colour
import numpy as np

# Klipp ColorPrimaries variant -> colour-science colourspace
PRIMARIES = [
    ("Bt2020", "ITU-R BT.2020"),
    ("Bt601_525", "SMPTE C"),
    ("Bt601_625", "ITU-R BT.470 - 625"),
    ("Bt470M", "ITU-R BT.470 - 525"),
    ("DciP3", "DCI-P3"),
    ("DisplayP3", "Display P3"),
    ("Ebu3213", "EBU Tech. 3213-E"),
    ("Smpte428", "CIE XYZ"),
    ("SGamut3", "S-Gamut3"),
    ("SGamut3Cine", "S-Gamut3.Cine"),
    ("VGamut", "V-Gamut"),
    ("CanonCinemaGamut", "Cinema Gamut"),
    ("AlexaWideGamut3", "ARRI Wide Gamut 3"),
    ("AlexaWideGamut4", "ARRI Wide Gamut 4"),
    ("RedWideGamut", "REDWideGamutRGB"),
    ("BmdWideGamut", "Blackmagic Wide Gamut"),
]


def matrix(name):
    target = colour.RGB_COLOURSPACES["ITU-R BT.709"]
    if name == "CIE XYZ":
        # SMPTE ST 428 is XYZ-encoded; taken as D65-referred XYZ.
        return np.asarray(target.matrix_XYZ_to_RGB)
    return colour.matrix_RGB_to_RGB(
        colour.RGB_COLOURSPACES[name], target, chromatic_adaptation_transform="Bradford"
    )


def main():
    path = os.path.join("crates", "klipp-color", "tests", "fixtures", "colour_primaries_to_bt709.csv")
    with open(path, "w") as f:
        f.write(f"# colour-science {colour.__version__}, generated {datetime.date.today().isoformat()}\n")
        f.write("# primaries, then the 3x3 source->BT.709 matrix row-major\n")
        for klipp, name in PRIMARIES:
            m = matrix(name)
            f.write(klipp + "," + ",".join(f"{v:.10f}" for v in m.flatten()) + "\n")
    for klipp, name in PRIMARIES:
        m = matrix(name)
        rows = ",\n            ".join(
            "[" + ", ".join(f"{v:.10}_f32" for v in r) + "]" for r in m
        )
        print(f"        // {name}\n        ColorPrimaries::{klipp} => [\n            {rows},\n        ],")


if __name__ == "__main__":
    main()
