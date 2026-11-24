# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import unittest

from report import evaluation_phase_span_milliseconds, render, verdicts


def run(end_to_end, statements=500, shards=4, jobs=1, phase_span=None):
    return {"evaluation_request_to_building_started_ms": end_to_end,
            "workers_evaluation_phase_span_ms": end_to_end if phase_span is None else phase_span,
            "statements": statements, "shards": shards, "jobs": jobs}


def ph(phase, start_ms, end_ms, offset_ms=0):
    return {"phase": phase, "start_ms": start_ms, "end_ms": end_ms, "offset_ms": offset_ms}


class ReportTest(unittest.TestCase):
    def test_speedup_below_target_fails(self):
        runs = {1: run(100_000), 2: run(90_000, jobs=2)}
        self.assertEqual(verdicts(runs), ["N=2: speedup 1.11 below 1.15"])

    def test_speedup_compares_end_to_end_durations_not_worker_phases(self):
        runs = {1: run(143_700, phase_span=104_900), 4: run(88_900, jobs=4, phase_span=78_400)}
        self.assertEqual(verdicts(runs), [])
        runs = {1: run(100_000, phase_span=200_000), 4: run(90_000, jobs=4, phase_span=50_000)}
        self.assertEqual(verdicts(runs), ["N=4: speedup 1.11 below 1.45"])

    def test_a_split_that_never_happened_fails(self):
        runs = {1: run(100_000), 2: run(50_000, jobs=1)}
        self.assertIn("N=2: ran on 1 job", verdicts(runs))

    def test_phase_span_covers_both_eval_phases_of_every_job(self):
        self.assertEqual(evaluation_phase_span_milliseconds([
            ph("fetch", 0, 50), ph("eval_flake", 50, 80), ph("eval_derivations", 90, 400),
            ph("eval_derivations", 120, 380)]), 350)

    def test_a_later_job_counts_from_its_own_dispatch(self):
        self.assertEqual(evaluation_phase_span_milliseconds([
            ph("eval_flake", 50, 80), ph("eval_derivations", 10, 400, offset_ms=100)]), 450)

    def test_the_small_flake_must_stay_on_one_job_and_not_slow_down_end_to_end(self):
        runs = {1: run(100_000)}
        small = {"standalone": run(10_000, phase_span=20_000), "multinode": run(12_000, jobs=2, phase_span=5_000)}
        self.assertEqual(verdicts(runs, small), [
            "small flake: 12000 ms with 4 workers up, 10000 ms standalone",
            "small flake: ran on 2 jobs",
        ])

    def test_small_flake_with_slow_worker_phase_span_and_fast_end_to_end_duration_passes_slowdown_check(self):
        small = {"standalone": run(10_000, phase_span=5_000), "multinode": run(10_500, phase_span=9_000)}
        self.assertEqual(verdicts({1: run(100_000)}, small), [])

    def test_rendered_summary_line_lists_end_to_end_duration_and_worker_phase_span_for_standalone_and_four_worker_runs(self):
        small = {"standalone": run(10_000, phase_span=5_000), "multinode": run(10_500, phase_span=9_000)}
        self.assertEqual(render({}, small).splitlines()[-1],
                         "small flake: end-to-end 10.0 s standalone, 10.5 s with 4 workers up on 1 job"
                         "  worker phase 5.0 s standalone, 9.0 s with 4 workers up")

    def test_a_half_measured_small_flake_renders_the_large_runs_only(self):
        runs = {1: run(100_000)}
        small = {"standalone": run(10_000)}
        self.assertEqual(verdicts(runs, small), [])
        self.assertNotIn("small flake", render(runs, small))

    def test_unmeasured_one_worker_run_has_no_speedup_verdict_and_lists_both_durations(self):
        runs = {4: run(90_000, jobs=1, phase_span=80_000)}
        self.assertEqual(verdicts(runs), [])
        self.assertEqual(render(runs, {}),
                         "N=4  end-to-end     90.0 s  worker phase     80.0 s"
                         "      500 statements     4 shards  1 job\n")

    def test_targets_met_report_nothing_and_render_every_run(self):
        runs = {n: run(100_000 // n, jobs=n) for n in range(1, 5)}
        small = {"standalone": run(10_000), "multinode": run(10_500)}
        self.assertEqual(verdicts(runs, small), [])
        text = render(runs, small)
        self.assertIn("4.00x", text)
        self.assertIn("small flake", text)


if __name__ == "__main__":
    unittest.main()
