#!/usr/bin/env python3
"""Sample a running Klipp's GPU and memory use over a soak test (#3404),
and say whether any of it keeps rising.

Every few seconds: the process's GPU memory (nvidia-smi), its open
/dev/nvidia* handles, its resident memory and its threads, written to a
CSV as they come. At the end, each measure's floor over the last third
of the run is set against its floor over the first third: a job's peak
comes and goes, a floor that climbs is held. Exits 1 when one rose past
its allowance, 0 otherwise.

    python3 scripts/soak_sample.py --pid "$(pgrep -x klipp-app)" --minutes 30 --out soak.csv

Without nvidia-smi (another vendor) the GPU memory column is empty and
not judged; the handles, memory and threads still are."""
import argparse
import csv
import os
from pathlib import Path
import subprocess
import sys
import time

# How far each measure's floor may rise before the run fails: the
# driver's pools grow in steps, and a project's caches settle.
ALLOWANCE = {"vram_mib": 256, "nvidia_handles": 8, "rss_mib": 512, "threads": 16}
FIELDS = ["t_secs", "vram_mib", "nvidia_handles", "rss_mib", "threads"]


def vram_mib(pid):
    """The process's GPU memory in MiB from nvidia-smi, None without it."""
    try:
        out = subprocess.run(["nvidia-smi", "--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"],
                             capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    total = 0
    for line in out.stdout.splitlines():
        fields = [f.strip() for f in line.split(",")]
        if len(fields) == 2 and fields[0] == str(pid) and fields[1].isdigit():
            total += int(fields[1])
    return total


def nvidia_handles(pid):
    """The driver's device files the process has open."""
    n = 0
    for fd in Path(f"/proc/{pid}/fd").iterdir():
        try:
            if os.readlink(fd).startswith("/dev/nvidia"):
                n += 1
        except OSError:
            pass
    return n


def status(pid):
    """Resident memory in MiB and thread count from /proc."""
    rss, threads = 0, 0
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1]) // 1024
        elif line.startswith("Threads:"):
            threads = int(line.split()[1])
    return rss, threads


def sample(pid, t):
    rss, threads = status(pid)
    return {"t_secs": round(t, 1), "vram_mib": vram_mib(pid), "nvidia_handles": nvidia_handles(pid), "rss_mib": rss, "threads": threads}


def judge(rows, allowance=ALLOWANCE):
    """Each measure's floor in the first and last thirds of `rows`, and
    the ones that rose past `allowance`: (report lines, failed names).
    A run too short to have thirds judges nothing."""
    lines, failed = [], []
    third = len(rows) // 3
    if third < 2:
        return ["too few samples to judge a trend"], []
    for key, allowed in allowance.items():
        values = [r[key] for r in rows if r.get(key) is not None]
        if len(values) < 6:
            lines.append(f"{key}: not measured")
            continue
        third = len(values) // 3
        first, last = min(values[:third]), min(values[-third:])
        rose = last - first
        verdict = "ok" if rose <= allowed else "RISING"
        if rose > allowed:
            failed.append(key)
        lines.append(f"{key}: floor {first} -> {last} (rose {rose}, allowed {allowed}), peak {max(values)}: {verdict}")
    return lines, failed


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pid", type=int, required=True, help="the running klipp-app")
    parser.add_argument("--minutes", type=float, default=30.0)
    parser.add_argument("--every", type=float, default=5.0, help="seconds between samples")
    parser.add_argument("--out", type=Path, default=Path("soak.csv"))
    args = parser.parse_args()
    rows, t0 = [], time.monotonic()
    with args.out.open("w", newline="") as f:
        out = csv.DictWriter(f, FIELDS)
        out.writeheader()
        while time.monotonic() - t0 < args.minutes * 60:
            if not Path(f"/proc/{args.pid}").exists():
                print(f"process {args.pid} ended after {time.monotonic() - t0:.0f} s", file=sys.stderr)
                break
            row = sample(args.pid, time.monotonic() - t0)
            rows.append(row)
            out.writerow(row)
            f.flush()
            time.sleep(args.every)
    lines, failed = judge(rows)
    print(f"{len(rows)} samples written to {args.out}")
    print("\n".join(lines))
    if failed:
        print(f"FAILED: {', '.join(failed)} kept rising", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
