#!/usr/bin/env python3
"""Generate colour-science reference values for camera log curves that
OpenColorIO has no built-in for.

Companion to gen_ocio_camera_log_reference.py. colour-science is the
colour-science community's reference implementation; before its values
were trusted here, it was checked against OCIO on the seven formats both
cover (S-Log3, LogC3, LogC4, V-Log, Canon Log 3, Log3G10, Apple Log) and
agreed to within 2e-5 stops under the conventions used below: input is a
normalised 10-bit code value, output is scene reflectance (0.18 = grey).

BMD Film Gen5 is in neither library and gets no fixture.

Requirements: pip install colour-science   (a venv is fine)
Usage (from the workspace root):
    python tools/gen_colour_camera_log_reference.py
"""

import datetime
import inspect
import os
import colour

OUT_DIR = os.path.join("crates", "klipp-color", "tests", "fixtures")

# Klipp LogFormat name -> colour-science LOG_DECODINGS key
FORMATS = {
    "SLog2": "S-Log2",
    "CanonLog": "Canon Log",
    "DJIDLog": "D-Log",
    "FLog": "F-Log",
    "NLog": "N-Log",
    "LeicaLLog": "L-Log",
    "GoProFlat": "Protune",
}


def main():
    stamp = datetime.date.today().isoformat()
    for klipp_name, key in FORMATS.items():
        fn = colour.LOG_DECODINGS[key]
        params = inspect.signature(fn).parameters
        kw = {}
        if "in_normalised_code_value" in params:
            kw["in_normalised_code_value"] = True
        if "out_reflection" in params:
            kw["out_reflection"] = True
        path = os.path.join(OUT_DIR, f"colour_camlog_{klipp_name}.csv")
        with open(path, "w") as f:
            f.write(f"# colour-science {colour.__version__} '{key}' {kw}, generated {stamp}\n")
            f.write("code,scene_linear\n")
            for i in range(1024):
                c = i / 1023.0
                f.write(f"{c:.9f},{float(fn(c, **kw)):.9g}\n")
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
