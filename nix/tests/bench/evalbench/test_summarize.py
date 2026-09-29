# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import pathlib
import shutil
import tarfile
import tempfile
import unittest

import summarize

FIXTURES = pathlib.Path(__file__).parent / "fixtures"


class Summarize(unittest.TestCase):
    def setUp(self):
        self.skewed = summarize.load_spans(FIXTURES / "skewed" / "trace")

    def test_a_truncated_last_line_is_skipped(self):
        self.assertEqual(sum(s["process"] == "worker" for s in self.skewed), 4)

    def test_the_worker_clock_skew_is_recovered_from_paired_messages(self):
        self.assertEqual(summarize.clock_offset(self.skewed), (250_000, True))

    def test_a_run_without_pairs_stays_unaligned(self):
        lonely = summarize.load_spans(FIXTURES / "lonely" / "trace")
        self.assertEqual(summarize.clock_offset(lonely), (0, False))

    def test_the_offset_moves_worker_and_eval_spans_onto_the_server_clock(self):
        trace = summarize.chrome_trace(self.skewed, 250_000)
        slices = {e["name"]: e for e in trace["traceEvents"] if e["ph"] == "X"}
        origin = min(e["ts"] for e in trace["traceEvents"] if e["ph"] == "X")
        self.assertEqual(slices["assign_job"]["ts"] - origin, 0)
        self.assertEqual(slices["job"]["ts"] - origin, 1_100)
        self.assertEqual(slices["resolve"]["ts"] - origin, 10_000)

    def test_overlapping_siblings_get_separate_lanes_and_children_share_their_parent_lane(self):
        trace = summarize.chrome_trace(self.skewed, 250_000)
        lanes = {}
        for e in trace["traceEvents"]:
            if e["ph"] == "X" and e["name"] in ("job", "query_known_derivations"):
                lanes.setdefault(e["name"], []).append(e["tid"])
        self.assertEqual(len(set(lanes["query_known_derivations"])), 2)
        self.assertIn(lanes["job"][0], lanes["query_known_derivations"])

    def test_every_process_is_named(self):
        trace = summarize.chrome_trace(self.skewed, 0)
        names = {e["args"]["name"] for e in trace["traceEvents"] if e["ph"] == "M"}
        self.assertEqual(names, {"server (11)", "worker (22)", "eval (33)"})

    def test_processes_of_different_vms_with_one_pid_stay_apart(self):
        spans = summarize.load_spans(FIXTURES / "samepid" / "trace")
        trace = summarize.chrome_trace(spans, 0)
        names = {e["args"]["name"] for e in trace["traceEvents"] if e["ph"] == "M"}
        self.assertEqual(names, {"server (7)", "worker (7)"})
        pids = {e["name"]: e["pid"] for e in trace["traceEvents"] if e["ph"] == "X"}
        self.assertNotEqual(pids["flush"], pids["wave"])

    def test_the_stage_table_is_ordered_by_total_time(self):
        table = summarize.stage_table(self.skewed)
        self.assertEqual(table[0]["name"], "job")
        known = next(r for r in table if r["name"] == "query_known_derivations")
        self.assertEqual(
            known,
            {"process": "worker", "name": "query_known_derivations", "count": 2,
             "total_ms": 6.0, "p50_ms": 3.0, "max_ms": 3.0},
        )
        self.assertEqual([r["total_ms"] for r in table],
                         sorted((r["total_ms"] for r in table), reverse=True))

    def test_summarize_writes_the_merged_trace_and_both_summaries(self):
        with tempfile.TemporaryDirectory() as out:
            out = pathlib.Path(out)
            shutil.copytree(FIXTURES / "skewed", out / "cold-clean")
            (out / "cold-clean" / "evaluation_metric.json").write_text(
                json.dumps([{"fetch_ms": 12, "eval_flake_ms": 30, "eval_drv_ms": 400}]))
            summarize.summarize(out, ["cold-clean"])

            run = json.loads((out / "summary.json").read_text())["cold-clean"]
            self.assertTrue(run["aligned"])
            self.assertEqual(run["offset_us"], 250_000)
            self.assertEqual(run["evaluation_metric"][0]["eval_drv_ms"], 400)
            self.assertTrue((out / "cold-clean" / "trace.json").exists())
            text = (out / "summary.txt").read_text()
            self.assertIn("cold-clean", text)
            self.assertIn("query_known_derivations", text)

    def test_publish_declares_the_bundle_and_both_summaries_as_build_products(self):
        with tempfile.TemporaryDirectory() as out:
            out = pathlib.Path(out)
            shutil.copytree(FIXTURES / "skewed", out / "cold-clean")
            summarize.summarize(out, ["cold-clean"])
            summarize.publish(out)

            lines = (out / "nix-support" / "hydra-build-products").read_text().splitlines()
            paths = [pathlib.Path(line.split()[2]) for line in lines]
            self.assertEqual([p.name for p in paths],
                             ["evalbench.tar.gz", "summary.txt", "summary.json"])
            self.assertTrue(all(p.is_absolute() and p.is_file() for p in paths))
            with tarfile.open(out / "evalbench.tar.gz") as bundle:
                names = bundle.getnames()
            self.assertIn("evalbench/cold-clean/trace.json", names)
            self.assertNotIn("evalbench/nix-support", names)


if __name__ == "__main__":
    unittest.main()
