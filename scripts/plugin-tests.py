#!/usr/bin/env python3
"""Build and exercise native plugin fixtures without using the editor installation."""
import argparse
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import resource
import shutil
import signal
import struct
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
SCENARIOS = ("identity", "channels", "alpha", "time")
CONTROLS = ("wrong-output", "passthrough", "missing", "render-error", "hang")
WIDTH, HEIGHT = 37, 23


def diagnostic(seed):
    rng = random.Random(seed)
    pixels = bytearray()
    for y in range(HEIGHT):
        for x in range(WIDTH):
            pixels.extend((x * 255 // (WIDTH - 1), y * 255 // (HEIGHT - 1),
                           rng.randrange(256) if (x + y) % 2 else 255,
                           (0, 1, 63, 127, 254, 255)[(x + y) % 6]))
    return bytes(pixels)


def expected_pixels(source, scenario, frame_time):
    # Independent oracle: deliberately not shared with the native fixture or host.
    pixels = [list(source[i:i + 4]) for i in range(0, len(source), 4)]
    if scenario == "channels":
        pixels = [[b, 255 - r, g, a] for r, g, b, a in pixels]
    elif scenario == "alpha":
        pixels = [[r, g, b, a // 2] for r, g, b, a in pixels]
    elif scenario == "time":
        pixels = [[(r + int(frame_time * 16)) % 256, g, b, a] for r, g, b, a in pixels]
    return bytes(value for pixel in pixels for value in pixel)


# frei0r: one plug-in per shared object, built from one fixture source.
FREI0R_KINDS = ("colour", "alpha", "params", "history", "bgra", "packed32", "neighbour")
FREI0R_WIDTH, FREI0R_HEIGHT = 40, 24  # frei0r takes sizes in multiples of 8
FREI0R_PARAMS = {"param-bool": [[0, {"Bool": True}]], "param-double": [[1, {"Double": 0.8}]],
                 "param-color": [[2, {"Color": [0.25, 0.5, 0.75]}]], "param-position": [[3, {"Position": [0.5, 0.25]}]],
                 "param-string": [[4, {"String": "bgr"}]]}
FREI0R_PARAMS["params-all"] = [p for value in FREI0R_PARAMS.values() for p in value]
# scenario: (plug-in kind, width, height, frame times, parameters)
FREI0R_SCENARIOS = {
    "colour": ("colour", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0, 0.5, 1.0, 3.25), []),
    "alpha": ("alpha", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0,), []),
    **{name: ("params", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0,), params) for name, params in FREI0R_PARAMS.items()},
    "history": ("history", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0, 0.04, 0.08), []),
    "bgra": ("bgra", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0,), []),
    "packed32": ("packed32", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0,), []),
    "neighbour": ("neighbour", FREI0R_WIDTH, FREI0R_HEIGHT, (0.0,), []),
    # Wider or taller than frei0r's 2048 limit, so the host must tile.
    "tile-wide": ("neighbour", 2064, 16, (0.0,), []),
    "tile-tall": ("neighbour", 16, 2064, (0.0,), []),
    # History kept per tile, frame to frame, across the seams.
    "tile-history": ("history", 2064, 16, (0.0, 0.04, 0.08), []),
    # Not a multiple of 8: padded by repeating the edges, rendered and
    # cropped back, a declared fallback, never the picture unchanged (#1469).
    "unaligned": ("colour", WIDTH, HEIGHT, (0.0,), []),
    # 1080p at half Playback Quality, and the grade-drag downsample: 540
    # isn't a multiple of 8. A geometry-reading case shows the padded frame.
    "half-size": ("colour", 960, 540, (0.0,), []),
    "half-size-cut": ("params", 960, 540, (0.0,), [[3, {"Position": [0.5, 0.25]}]]),
}
FREI0R_CONTROLS = ("wrong-output", "passthrough", "missing", "hang", "no-trace")
# Scenarios the host fails today, each with its issue and the failure it shows.
# A pin that starts passing fails the suite until it is removed here.
FREI0R_PINS = {}


def diagnostic_sized(seed, width, height):
    rng = random.Random(seed)
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            pixels.extend((x * 255 // (width - 1), y * 255 // (height - 1),
                           rng.randrange(256) if (x + y) % 2 else 255,
                           (0, 1, 63, 127, 254, 255)[(x + y) % 6]))
    return bytes(pixels)


def f32(value):
    return struct.unpack("f", struct.pack("f", value))[0]


def clamp8(value):
    return 0 if value <= 0 else 255 if value >= 255 else int(value + 0.5)


def padded(value):
    return max(8, -(-value // 8) * 8)


def frei0r_expected(scenario, sources, frame, frame_time, width, height):
    """The expected frame. A size frei0r can't take is worked on the frame
    padded by repeating its last column and row, then cropped back."""
    pw, ph = padded(width), padded(height)
    if (pw, ph) == (width, height):
        return frei0r_expected_aligned(scenario, sources, frame, frame_time, width, height)
    def pad(source):
        rows = [source[y * width * 4:(y + 1) * width * 4] for y in range(height)]
        rows = [row + row[-4:] * (pw - width) for row in rows]
        return b"".join(rows + [rows[-1]] * (ph - height))
    out = frei0r_expected_aligned(scenario, [pad(s) for s in sources], frame, frame_time, pw, ph)
    return b"".join(out[y * pw * 4:y * pw * 4 + width * 4] for y in range(height))


def frei0r_expected_aligned(scenario, sources, frame, frame_time, width, height):
    # Independent oracle: deliberately not shared with the native fixture or host.
    source = sources[frame]
    pixels = [list(source[i:i + 4]) for i in range(0, len(source), 4)]
    kind, params = FREI0R_SCENARIOS[scenario][0], dict(FREI0R_SCENARIOS[scenario][4])
    if kind == "colour":
        pixels = [[(b + int(frame_time * 16)) % 256, 255 - r, g, a] for r, g, b, a in pixels]
    elif kind == "alpha":
        pixels = [[r, g, b, a // 2] for r, g, b, a in pixels]
    elif kind == "params":
        invert = params.get(0, {}).get("Bool", False)
        gain = params.get(1, {}).get("Double", 0.5)
        tint = [f32(c) for c in params.get(2, {}).get("Color", [1.0, 1.0, 1.0])]
        cut = params.get(3, {}).get("Position", [1.0, 1.0])
        order = params.get(4, {}).get("String", "rgb")
        out = []
        for index, (r, g, b, a) in enumerate(pixels):
            x, y = index % width, index // width
            r = 255 - r if invert else r
            g = clamp8(g * gain * 2.0)
            r, g, b = clamp8(r * tint[0]), clamp8(g * tint[1]), clamp8(b * tint[2])
            a = 0 if x >= cut[0] * width and y >= cut[1] * height else a
            out.append([b, g, r, a] if order == "bgr" else [r, g, b, a])
        pixels = out
    elif kind == "history":
        previous = sources[max(frame - 1, 0)]
        pixels = [list(previous[i:i + 4]) for i in range(0, len(previous), 4)]
    elif kind == "bgra":
        pixels = [[r, g, 255 - b, a] for r, g, b, a in pixels]
    elif kind == "packed32":
        pixels = [[255 - r, g, b, a] for r, g, b, a in pixels]
    elif kind == "neighbour":
        out = []
        for y in range(height):
            rows = [min(max(y + dy, 0), height - 1) * width for dy in (-1, 0, 1)]
            for x in range(width):
                columns = [min(max(x + dx, 0), width - 1) for dx in (-1, 0, 1)]
                out.append([(sum(pixels[row + column][c] for row in rows for column in columns) + 4) // 9 for c in range(4)])
        pixels = out
    return bytes(value for pixel in pixels for value in pixel)


def run_frei0r_case(args, host, plugins, output, scenario, control, index):
    case = output / f"{index:02d}-{scenario}-{control}"
    case.mkdir()
    environment = os.environ.copy()
    for key in ("HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"):
        directory = case / key.lower()
        directory.mkdir()
        environment[key] = str(directory)
    # A duplicate of every fixture where frei0r discovery looks by default, so
    # a host that fell back to discovery would pass the missing-plug-in control.
    default_plugins = Path(environment["HOME"]) / ".frei0r-1/lib"
    default_plugins.parent.mkdir()
    default_plugins.symlink_to(plugins, target_is_directory=True)
    environment["FREI0R_PATH"] = str(plugins)
    environment["KLIPP_FIXTURE_MODE"] = control
    kind, width, height, times, params = FREI0R_SCENARIOS[scenario]
    name = f"klipp-fixture-{kind}"
    params_path = case / "params.json"
    params_path.write_text(json.dumps(params))
    # Each frame of a multi-frame scenario has its own picture, so carried state shows.
    sources = [diagnostic_sized(args.seed + frame, width, height) for frame in range(len(times))]
    command = [str(host), str(plugins / (f"{name}.so" if control != "missing" else "absent.so")),
               name, str(width), str(height), str(params_path)]
    for frame, frame_time in enumerate(times):
        (case / f"{frame}-input.rgba").write_bytes(sources[frame])
        command += [repr(frame_time), str(case / f"{frame}-input.rgba"), str(case / f"{frame}-actual.rgba")]
    result = {"scenario": scenario, "control": control, "status": "passed",
              "reason": "", "frames": [], "artifacts": str(case)}
    started = time.monotonic()
    log_path = case / "host.log"
    timeout = min(args.timeout, 0.5) if control == "hang" else args.timeout
    with log_path.open("wb") as log:
        child = subprocess.Popen(command, stdout=log, stderr=log, env=environment,
                                 start_new_session=True, preexec_fn=limits(timeout, args.memory_mib))
        try:
            code = child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            result.update(status="failed", reason="timeout")
            code = None
    log = log_path.read_bytes()
    if code:
        reason = ("missing-plugin" if b"required fixture missing" in log else
                  "refused" if code == 3 and b"KLIPP_HOST_REFUSED" in log else "worker-failed")
        result.update(status="failed", reason=reason, exit_code=code)
    elif code == 0:
        renders = [(float(t), int(w), int(h)) for n, t, w, h in
                   re.findall(rb"KLIPP_FIXTURE_RENDER (\S+) (\S+) (\d+)x(\d+)\n", log) if n.decode() == name]
        for frame, frame_time in enumerate(times):
            actual_path = case / f"{frame}-actual.rgba"
            actual = actual_path.read_bytes() if actual_path.exists() else b""
            frame_renders = [(w, h) for t, w, h in renders if t == frame_time]
            entry = {"time": frame_time, "actual": str(actual_path), "renders": frame_renders}
            result["frames"].append(entry)
            expected = frei0r_expected(scenario, sources, frame, frame_time, width, height)
            (case / f"{frame}-expected.rgba").write_bytes(expected)
            entry["mismatched_bytes"] = sum(a != b for a, b in zip(actual, expected)) + abs(len(actual) - len(expected))
            if entry["mismatched_bytes"]:
                result.update(status="failed", reason="wrong-output")
                break
            if not frame_renders:
                result.update(status="failed", reason="render-not-observed")
                break
            # A padded size declares its fallback; any other size doesn't.
            pw, ph = padded(width), padded(height)
            fallback = f"KLIPP_HOST_FALLBACK padded {width}x{height} {pw}x{ph}\n".encode()
            if (pw, ph) != (width, height) and fallback not in log:
                result.update(status="failed", reason="fallback-not-declared")
                break
            if (pw, ph) == (width, height) and b"KLIPP_HOST_FALLBACK" in log:
                result.update(status="failed", reason="undeclared-fallback")
                break
            if scenario.startswith("tile-") and (len(frame_renders) < 2 or
                                                 any(w > 2048 or h > 2048 for w, h in frame_renders)):
                result.update(status="failed", reason="not-tiled")
                break
    result["elapsed_seconds"] = time.monotonic() - started
    return result


def c_compiler():
    """The fixtures' C compiler; a missing one stops the run with what to install, never a skip."""
    compiler = os.environ.get("CC", "cc")
    if shutil.which(compiler) is None:
        raise ValueError(f"C compiler '{compiler}' not found: the plug-in fixtures are C sources. "
                         "Install one (gcc, from the base-devel group on Arch) or set CC.")
    return compiler


def frei0r_suite(args, output, build_env, report):
    host = args.host.resolve() if args.host else None
    if host is None:
        target = Path(build_env.get("CARGO_TARGET_DIR", str(ROOT / "target"))).resolve()
        if target.is_relative_to("/tmp"):
            raise ValueError("CARGO_TARGET_DIR must not be in /tmp")
        with (output / "host-build.log").open("wb") as log:
            subprocess.run(["cargo", "build", "--offline", "-p", "klipp-frei0r", "--bin", "klipp-frei0r-test-host", "--target-dir", str(target)],
                           cwd=ROOT, env=build_env, stdout=log, stderr=log, check=True, timeout=600)
        host = target / "debug/klipp-frei0r-test-host"
    if not host.is_file():
        raise ValueError(f"host executable absent: {host}")
    plugins = args.plugin_root.resolve() if args.plugin_root else output / "plugins"
    if args.plugin_root is None:
        plugins.mkdir()
        with (output / "fixture-build.log").open("wb") as log:
            for number, kind in enumerate(FREI0R_KINDS):
                subprocess.run([c_compiler(), "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC",
                                f"-DKLIPP_KIND={number}", str(ROOT / "crates/klipp-frei0r/tests/fixtures/frei0r/fixture.c"),
                                "-o", str(plugins / f"klipp-fixture-{kind}.so")],
                               env=build_env, stdout=log, stderr=log, check=True, timeout=60)
    scenarios = tuple(FREI0R_SCENARIOS) if args.scenario == "all" else (args.scenario,)
    cases = [(scenario, args.control, None) for scenario in scenarios]
    if args.self_test:
        cases += [("colour", mode, reason) for mode, reason in
                  (("wrong-output", "wrong-output"), ("passthrough", "wrong-output"),
                   ("missing", "missing-plugin"), ("hang", "timeout"), ("no-trace", "render-not-observed"))]
    for index, (scenario, control, rejection) in enumerate(cases):
        result = run_frei0r_case(args, host, plugins, output, scenario, control, index)
        if rejection:
            result["expected_rejection"] = rejection
            result["control_detected"] = result["status"] == "failed" and result["reason"] == rejection
        elif control == "normal" and scenario in FREI0R_PINS:
            issue, failure = FREI0R_PINS[scenario]
            result["pinned"] = issue
            # Fails the way the issue records: expected for now. Any other outcome,
            # passing included, needs the pin looked at.
            result["pin_holds"] = result["status"] == "failed" and result["reason"] == failure
        report["results"].append(result)
    return all(r.get("control_detected", r.get("pin_holds", r["status"] == "passed")) for r in report["results"])


def limits(seconds, memory_mib):
    def apply():
        resource.setrlimit(resource.RLIMIT_AS, (memory_mib * 1024**2,) * 2)
        resource.setrlimit(resource.RLIMIT_CPU, (math.ceil(seconds) + 1,) * 2)
        resource.setrlimit(resource.RLIMIT_FSIZE, (8 * 1024**2,) * 2)
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    return apply


def run_case(args, host, plugins, output, scenario, control, index):
    case = output / f"{index:02d}-{scenario}-{control}"
    case.mkdir()
    environment = os.environ.copy()
    for key in ("HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"):
        directory = case / key.lower()
        directory.mkdir()
        environment[key] = str(directory)
    # A duplicate bundle in the normal HOME and environment paths catches any
    # regression that accidentally falls back to default discovery.
    default_plugins = Path(environment["HOME"]) / ".OFX/Plugins"
    default_plugins.parent.mkdir()
    default_plugins.symlink_to(plugins, target_is_directory=True)
    environment["OFX_PLUGIN_PATH"] = str(plugins)
    environment["KLIPP_FIXTURE_MODE"] = control
    source = diagnostic(args.seed)
    input_path = case / "input.rgba"
    input_path.write_bytes(source)
    result = {"scenario": scenario, "control": control, "status": "passed",
              "reason": "", "frames": [], "artifacts": str(case)}
    started = time.monotonic()
    times = (0.0, 0.5, 1.0, 3.25) if scenario == "time" else (0.0,)
    for frame, frame_time in enumerate(times):
        actual_path = case / f"{frame}-actual.rgba"
        expected = expected_pixels(source, scenario, frame_time)
        (case / f"{frame}-expected.rgba").write_bytes(expected)
        log_path = case / f"{frame}-host.log"
        search = case / "missing-plugins" if control == "missing" else plugins
        command = [str(host), str(search), f"org.klipp.fixture.{scenario}", str(input_path),
                   str(actual_path), str(WIDTH), str(HEIGHT), str(frame_time)]
        timeout = min(args.timeout, 0.5) if control == "hang" else args.timeout
        with log_path.open("wb") as log:
            child = subprocess.Popen(command, stdout=log, stderr=log, env=environment,
                                     start_new_session=True, preexec_fn=limits(timeout, args.memory_mib))
            try:
                code = child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                result.update(status="failed", reason="timeout")
                break
        if code:
            reason = "missing-plugin" if b"required fixture missing" in log_path.read_bytes() else "worker-failed"
            result.update(status="failed", reason=reason, exit_code=code)
            break
        actual = actual_path.read_bytes() if actual_path.exists() else b""
        mismatch = sum(a != b for a, b in zip(actual, expected)) + abs(len(actual) - len(expected))
        result["frames"].append({"time": frame_time, "mismatched_bytes": mismatch,
                                 "actual": str(actual_path), "log": str(log_path)})
        if mismatch:
            result.update(status="failed", reason="wrong-output")
            break
        marker = f"KLIPP_FIXTURE_RENDER {scenario} {frame_time:g}\n".encode()
        if marker not in log_path.read_bytes():
            result.update(status="failed", reason="render-not-observed")
            break
    result["elapsed_seconds"] = time.monotonic() - started
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--format", choices=("ofx", "frei0r", "lv2", "clap"), default="ofx")
    parser.add_argument("--backend", choices=("cpu", "gpu"), default="cpu")
    parser.add_argument("--scenario", choices=("all",) + SCENARIOS + tuple(FREI0R_SCENARIOS), default="all")
    parser.add_argument("--seed", type=int, default=932)
    parser.add_argument("--control", choices=("normal",) + tuple(dict.fromkeys(CONTROLS + FREI0R_CONTROLS)), default="normal")
    parser.add_argument("--self-test", action="store_true", help="also prove negative controls are rejected")
    parser.add_argument("--host", type=Path, help="prebuilt klipp-plugin-test-host (otherwise build offline)")
    parser.add_argument("--plugin-root", type=Path, help="use an existing fixture bundle directory; never auto-fill it")
    parser.add_argument("--output", type=Path, help="new artifact directory outside the repository")
    parser.add_argument("--timeout", type=float, default=15.0, help="wall-clock seconds per frame (0 < n <= 60)")
    parser.add_argument("--memory-mib", type=int, default=512, help="worker address-space limit (128–4096 MiB)")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 60 or not 128 <= args.memory_mib <= 4096:
        parser.error("invalid time or memory limit")
    output = (args.output or Path.home() / ".cache/klipp-tmp/plugin-tests" / uuid.uuid4().hex).resolve()
    if any((parent / ".git").exists() for parent in (output, *output.parents)) or output.is_relative_to("/tmp"):
        parser.error("artifacts must be outside the repository and /tmp; use ~/.cache/klipp-tmp")
    output.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": 1, "format": args.format, "backend": args.backend, "seed": args.seed,
              "status": "failed", "results": [], "artifacts": str(output),
              "tolerance_bytes": 0, "limits": {"seconds_per_frame": args.timeout, "memory_mib": args.memory_mib}}
    started = time.monotonic()
    exit_code = 1
    try:
        frei0r = args.format == "frei0r"
        if args.format not in ("ofx", "frei0r") or args.backend != "cpu" or platform.system() != "Linux" or platform.machine() != "x86_64":
            report.update(status="unsupported", reason="This suite implements CPU OFX and frei0r on Linux x86-64; other formats/backends need separate fixtures.")
            exit_code = 2
        elif (args.scenario not in ("all",) + (tuple(FREI0R_SCENARIOS) if frei0r else SCENARIOS) or
              args.control not in ("normal",) + (FREI0R_CONTROLS if frei0r else CONTROLS)):
            report.update(status="unsupported", reason=f"scenario or control not in the {args.format} suite")
            exit_code = 2
        elif frei0r:
            build_env = os.environ.copy()
            build_env["TMPDIR"] = str(output)
            passed = frei0r_suite(args, output, build_env, report)
            report["status"] = "passed" if passed else "failed"
            exit_code = 0 if passed else 1
        else:
            host = args.host.resolve() if args.host else None
            build_env = os.environ.copy()
            build_env["TMPDIR"] = str(output)
            if host is None:
                target = Path(build_env.get("CARGO_TARGET_DIR", str(ROOT / "target"))).resolve()
                if target.is_relative_to("/tmp"):
                    raise ValueError("CARGO_TARGET_DIR must not be in /tmp")
                with (output / "host-build.log").open("wb") as log:
                    subprocess.run(["cargo", "build", "--offline", "-p", "klipp-plugin", "--bin", "klipp-plugin-test-host", "--target-dir", str(target)],
                                   cwd=ROOT, env=build_env, stdout=log, stderr=log, check=True, timeout=600)
                host = target / "debug/klipp-plugin-test-host"
            if not host.is_file():
                raise ValueError(f"host executable absent: {host}")
            plugins = args.plugin_root.resolve() if args.plugin_root else output / "plugins"
            if args.plugin_root is None:
                binary = plugins / "KlippFixture.ofx.bundle/Contents/Linux-x86-64/KlippFixture.ofx"
                binary.parent.mkdir(parents=True)
                with (output / "fixture-build.log").open("wb") as log:
                    subprocess.run([c_compiler(), "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC",
                                    str(ROOT / "crates/klipp-plugin/tests/fixtures/ofx/fixture.c"), "-o", str(binary)],
                                   env=build_env, stdout=log, stderr=log, check=True, timeout=60)
            scenarios = SCENARIOS if args.scenario == "all" else (args.scenario,)
            cases = [(scenario, args.control, None) for scenario in scenarios]
            if args.self_test:
                cases += [("channels", mode, reason) for mode, reason in
                          (("wrong-output", "wrong-output"), ("passthrough", "wrong-output"),
                           ("missing", "missing-plugin"), ("render-error", "wrong-output"), ("hang", "timeout"))]
                cases += [("identity", "render-error", "render-not-observed")]
            for index, (scenario, control, rejection) in enumerate(cases):
                result = run_case(args, host, plugins, output, scenario, control, index)
                if rejection:
                    result["expected_rejection"] = rejection
                    result["control_detected"] = result["status"] == "failed" and result["reason"] == rejection
                report["results"].append(result)
            passed = all(r.get("control_detected", r["status"] == "passed") for r in report["results"])
            report["status"] = "passed" if passed else "failed"
            exit_code = 0 if passed else 1
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        report["reason"] = str(error)
    report["elapsed_seconds"] = time.monotonic() - started
    rendered = json.dumps(report, indent=2) + "\n"
    (output / "result.json").write_text(rendered)
    print(rendered, end="")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
