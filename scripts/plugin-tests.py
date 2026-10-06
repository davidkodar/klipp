#!/usr/bin/env python3
"""Build and exercise native plugin fixtures without using the editor installation."""
import argparse
import json
import math
import os
from pathlib import Path
import platform
import random
import resource
import signal
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
    parser.add_argument("--format", choices=("ofx", "lv2", "clap"), default="ofx")
    parser.add_argument("--backend", choices=("cpu", "gpu"), default="cpu")
    parser.add_argument("--scenario", choices=("all",) + SCENARIOS, default="all")
    parser.add_argument("--seed", type=int, default=932)
    parser.add_argument("--control", choices=("normal",) + CONTROLS, default="normal")
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
        if args.format != "ofx" or args.backend != "cpu" or platform.system() != "Linux" or platform.machine() != "x86_64":
            report.update(status="unsupported", reason="This suite implements CPU OFX on Linux x86-64; other formats/backends need separate fixtures.")
            exit_code = 2
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
                    subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC",
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
