#!/usr/bin/env python3
"""The soak test's verdict (#3404): a floor that climbs fails, a job's
peak that comes and goes doesn't; and a sample of this process reads."""

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from soak_sample import judge, sample  # noqa: E402


def run(vram):
    """Samples with `vram` as their GPU memory, the rest flat."""
    return [{"vram_mib": v, "nvidia_handles": 36, "rss_mib": 900, "threads": 60} for v in vram]


class JudgeTests(unittest.TestCase):
    def test_a_flat_run_with_job_peaks_passes(self):
        # Bounces and exports peak and let go; the floor stays.
        lines, failed = judge(run([600, 1100, 600, 2400, 610, 600, 1700, 605, 600, 2300, 610, 600]))
        self.assertEqual(failed, [], lines)

    def test_a_rising_floor_fails(self):
        # The 0.3.0 shape: every decoder left a context behind.
        lines, failed = judge(run([600, 900, 1200, 1500, 1800, 2100, 2400, 2700, 3000, 3300, 3600, 3900]))
        self.assertEqual(failed, ["vram_mib"])
        self.assertTrue(any("RISING" in line for line in lines), lines)

    def test_climbing_handles_fail_even_with_memory_flat(self):
        rows = run([600] * 12)
        for i, r in enumerate(rows):
            r["nvidia_handles"] = 36 + 3 * i
        self.assertEqual(judge(rows)[1], ["nvidia_handles"])

    def test_no_gpu_reading_is_not_judged(self):
        lines, failed = judge(run([None] * 12))
        self.assertEqual(failed, [])
        self.assertIn("vram_mib: not measured", lines)

    def test_a_short_run_judges_nothing(self):
        self.assertEqual(judge(run([600, 5000, 9000])), (["too few samples to judge a trend"], []))


class SampleTests(unittest.TestCase):
    def test_this_process_samples(self):
        row = sample(os.getpid(), 0.0)
        self.assertGreater(row["rss_mib"], 0)
        self.assertGreater(row["threads"], 0)
        self.assertGreaterEqual(row["nvidia_handles"], 0)


if __name__ == "__main__":
    unittest.main()
