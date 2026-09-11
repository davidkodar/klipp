#!/usr/bin/env python3
"""Generate OCIO reference values for klipp-color's camera log curves.

Every log format Klipp offers has to turn a camera's encoded signal into
scene-linear light (0.18 = mid grey). This script asks OpenColorIO — which
ships the manufacturers' published transforms as built-ins — what the
right answer is for each code value, and writes CSV fixtures that
`tests/camera_log_parity.rs` checks Klipp's curves against without OCIO
installed.

Two kinds of built-in are used:
  * "CURVE - X_to_LINEAR": the transfer curve alone.
  * "CAMERA_to_ACES2065-1": curve + camera gamut -> ACES AP0. For those
    the input is *neutral* (R = G = B): every gamut matrix maps neutral to
    neutral (white to white, via the D65 -> D60 adaptation), so the output
    channel is exactly the curve. That isolates the curve from the gamut.

Formats with no OCIO built-in (S-Log2, Canon Log 1, BMD Film Gen5, DJI
D-Log, Fuji F-Log, Nikon N-Log, GoPro Flat, Leica L-Log) get no fixture:
the parity test reports them as unverified rather than trusting a second
hand-written copy of the same formula.

Requirements: OpenColorIO >= 2.3 with Python bindings.
Usage (from the workspace root):
    python tools/gen_ocio_camera_log_reference.py
"""

import datetime
import os
import PyOpenColorIO as ocio

OUT_DIR = os.path.join("crates", "klipp-color", "tests", "fixtures")

# Klipp LogFormat name -> (OCIO built-in, is_curve_only)
FORMATS = {
    "SLog3":      ("SONY_SLOG3-SGAMUT3_to_ACES2065-1", False),
    "LogC3":      ("ARRI_ALEXA-LOGC-EI800-AWG_to_ACES2065-1", False),
    "LogC4":      ("ARRI_LOGC4_to_ACES2065-1", False),
    "VLog":       ("PANASONIC_VLOG-VGAMUT_to_ACES2065-1", False),
    "CanonLog3":  ("CURVE - CANON_CLOG3_to_LINEAR", True),
    "RedLog3G10": ("RED_LOG3G10-RWG_to_ACES2065-1", False),
    "AppleLog":   ("CURVE - APPLE_LOG_to_LINEAR", True),
}

# Every 10-bit code value: log footage is 10-bit, and the curves' toes,
# breakpoints and shoulders all fall between the round numbers.
CODES = [i / 1023.0 for i in range(1024)]


def main():
    cfg = ocio.Config.CreateRaw()
    stamp = datetime.date.today().isoformat()
    for klipp_name, (builtin, curve_only) in FORMATS.items():
        t = ocio.BuiltinTransform(builtin)
        cpu = cfg.getProcessor(t).getDefaultCPUProcessor()
        path = os.path.join(OUT_DIR, f"ocio_camlog_{klipp_name}.csv")
        with open(path, "w") as f:
            f.write(f"# OCIO {ocio.__version__} built-in '{builtin}', generated {stamp}\n")
            f.write("# neutral input, curve isolated\n" if not curve_only else "# curve only\n")
            f.write("code,scene_linear\n")
            for c in CODES:
                r, g, b = cpu.applyRGB([c, c, c])
                if not curve_only:
                    # Neutral in must be neutral out, or the isolation
                    # argument above does not hold for this transform.
                    assert abs(r - g) < 1e-4 * max(1.0, abs(r)) and abs(r - b) < 1e-4 * max(1.0, abs(r)), \
                        f"{builtin}: neutral input came out non-neutral at {c}: {r} {g} {b}"
                f.write(f"{c:.9f},{r:.9g}\n")
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
