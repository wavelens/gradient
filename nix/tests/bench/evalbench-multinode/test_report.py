# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import unittest

from report import render, verdicts, wall_ms


def run(wall, statements=500, shards=4, jobs=1):
    return {"wall_ms": wall, "statements": statements, "shards": shards, "jobs": jobs}


def ph(phase, start_ms, end_ms, offset_ms=0):
    return {"phase": phase, "start_ms": start_ms, "end_ms": end_ms, "offset_ms": offset_ms}


class ReportTest(unittest.TestCase):
    def test_speedup_below_target_fails(self):
        runs = {1: run(100_000), 2: run(70_000, jobs=2)}
        self.assertEqual(verdicts(runs), ["N=2: speedup 1.43 below 1.60"])

    def test_a_split_that_never_happened_fails(self):
        runs = {1: run(100_000), 2: run(50_000, jobs=1)}
        self.assertIn("N=2: ran on 1 job", verdicts(runs))

    def test_statements_growing_with_attributes_fail(self):
        runs = {1: run(100_000, statements=500), 2: run(50_000, statements=5_000, shards=8, jobs=2)}
        self.assertIn("N=2: 4500 extra statements for 8 shards", verdicts(runs))

    def test_wall_clock_spans_both_eval_phases_of_every_job(self):
        self.assertEqual(wall_ms([ph("fetch", 0, 50), ph("eval_flake", 50, 80), ph("eval_derivations", 90, 400),
                                  ph("eval_derivations", 120, 380)]), 350)

    def test_a_later_job_counts_from_its_own_dispatch(self):
        self.assertEqual(wall_ms([ph("eval_flake", 50, 80), ph("eval_derivations", 10, 400, offset_ms=100)]), 450)

    def test_the_small_flake_must_stay_on_one_job_and_not_slow_down(self):
        runs = {1: run(100_000)}
        small = {"standalone": run(10_000), "multinode": run(12_000, jobs=2)}
        self.assertEqual(verdicts(runs, small), [
            "small flake: 12000 ms with 4 workers up, 10000 ms standalone",
            "small flake: ran on 2 jobs",
        ])

    def test_targets_met_report_nothing_and_render_every_run(self):
        runs = {n: run(100_000 // n, jobs=n) for n in range(1, 5)}
        small = {"standalone": run(10_000), "multinode": run(10_500)}
        self.assertEqual(verdicts(runs, small), [])
        text = render(runs, small)
        self.assertIn("4.00x", text)
        self.assertIn("small flake", text)


if __name__ == "__main__":
    unittest.main()
