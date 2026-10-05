#!/usr/bin/env python3
"""How many jobs a gate build runs: half the cores, within free memory."""

import subprocess
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "gate-jobs.sh"


def jobs(cores, available_mb):
    out = subprocess.run(["sh", str(SCRIPT), str(cores), str(available_mb)], capture_output=True, text=True, check=True, stdin=subprocess.DEVNULL)
    return int(out.stdout.strip())


class GateJobsTests(unittest.TestCase):
    def test_half_the_cores_when_memory_allows(self):
        self.assertEqual(jobs(32, 64_000), 16)
        self.assertEqual(jobs(8, 64_000), 4)

    def test_memory_caps_it_at_two_and_a_half_gigabytes_a_job(self):
        # What the machine had on 2026-10-05: 32 cores, 9 GB free.
        self.assertEqual(jobs(32, 9_000), 3)
        self.assertEqual(jobs(32, 25_600), 10)

    def test_never_fewer_than_two(self):
        self.assertEqual(jobs(2, 64_000), 2)
        self.assertEqual(jobs(32, 1_000), 2)

    def test_reads_the_machine_when_not_told(self):
        out = subprocess.run(["sh", str(SCRIPT)], capture_output=True, text=True, check=True, stdin=subprocess.DEVNULL)
        self.assertGreaterEqual(int(out.stdout.strip()), 2)


if __name__ == "__main__":
    unittest.main()
