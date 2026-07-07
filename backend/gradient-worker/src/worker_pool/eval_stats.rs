/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

// The wire-level delta and the metrics toggle live in the shared evaluator crate.
pub use gradient_eval::stats::{StatsDelta, metrics_enabled};

/// Environment the eval-worker subprocess needs for thunk/function-call counts.
/// libnixexpr's `Counter`s are no-ops unless `NIX_SHOW_STATS` is set (otherwise
/// `total_thunks`/`fn_calls` always report 0); the stats dump it would print is
/// routed to `/dev/null` so it cannot pollute the worker's stderr.
pub(crate) fn eval_worker_stats_env(
    metrics_enabled: bool,
) -> &'static [(&'static str, &'static str)] {
    if metrics_enabled {
        &[
            ("NIX_SHOW_STATS", "1"),
            ("NIX_SHOW_STATS_PATH", "/dev/null"),
        ]
    } else {
        &[]
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EntryPointCost {
    pub attr: String,
    pub thunks: u64,
    pub fn_calls: u64,
    pub alloc_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EvalStatsTotals {
    pub total_thunks: u64,
    pub fn_calls: u64,
    pub primop_calls: u64,
    pub lookups: u64,
    pub alloc_bytes: u64,
    pub peak_heap_bytes: u64,
    pub peak_rss_bytes: u64,
    pub per_entry_point: Vec<EntryPointCost>,
}

/// Accumulates per-request deltas into eval-wide totals, per-entry-point
/// buckets, and peak gauges.
#[derive(Debug, Default)]
pub(crate) struct EvalStatsAccumulator {
    totals: EvalStatsTotals,
    buckets: HashMap<String, EntryPointCost>,
}

impl EvalStatsAccumulator {
    pub fn observe(&mut self, entry_point: &str, delta: StatsDelta, rss_bytes: u64) {
        self.totals.total_thunks += delta.nr_thunks;
        self.totals.fn_calls += delta.nr_function_calls;
        self.totals.primop_calls += delta.nr_primop_calls;
        self.totals.lookups += delta.nr_lookups;
        self.totals.alloc_bytes += delta.alloc_bytes;
        self.totals.peak_heap_bytes = self.totals.peak_heap_bytes.max(delta.gc_heap_size);
        self.totals.peak_rss_bytes = self.totals.peak_rss_bytes.max(rss_bytes);

        let b = self
            .buckets
            .entry(entry_point.to_string())
            .or_insert_with(|| EntryPointCost {
                attr: entry_point.to_string(),
                ..Default::default()
            });
        b.thunks += delta.nr_thunks;
        b.fn_calls += delta.nr_function_calls;
        b.alloc_bytes += delta.alloc_bytes;
    }

    pub fn finish(mut self) -> EvalStatsTotals {
        self.totals.per_entry_point = self.buckets.into_values().collect();
        self.totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(thunks: u64, fn_calls: u64, alloc: u64) -> StatsDelta {
        StatsDelta {
            nr_thunks: thunks,
            nr_function_calls: fn_calls,
            nr_primop_calls: 0,
            alloc_bytes: alloc,
            gc_heap_size: alloc,
            ..Default::default()
        }
    }

    #[test]
    fn sums_totals_across_requests() {
        let mut acc = EvalStatsAccumulator::default();
        acc.observe("packages.x86_64-linux.a", d(10, 2, 100), 500);
        acc.observe("packages.x86_64-linux.a", d(5, 1, 50), 700);
        let r = acc.finish();
        assert_eq!(r.total_thunks, 15);
        assert_eq!(r.fn_calls, 3);
        assert_eq!(r.alloc_bytes, 150);
        assert_eq!(r.peak_rss_bytes, 700, "peak RSS is the max observed");
    }

    #[test]
    fn buckets_per_entry_point() {
        let mut acc = EvalStatsAccumulator::default();
        acc.observe("a", d(10, 0, 0), 100);
        acc.observe("b", d(4, 0, 0), 100);
        acc.observe("a", d(6, 0, 0), 100);
        let mut costs = acc.finish().per_entry_point;
        costs.sort_by(|x, y| x.attr.cmp(&y.attr));
        assert_eq!(
            costs
                .iter()
                .map(|c| (c.attr.as_str(), c.thunks))
                .collect::<Vec<_>>(),
            vec![("a", 16), ("b", 4)]
        );
    }

    #[test]
    fn heap_peak_is_max_gauge_not_sum() {
        let mut acc = EvalStatsAccumulator::default();
        acc.observe(
            "a",
            StatsDelta {
                gc_heap_size: 900,
                ..d(0, 0, 0)
            },
            0,
        );
        acc.observe(
            "a",
            StatsDelta {
                gc_heap_size: 300,
                ..d(0, 0, 0)
            },
            0,
        );
        assert_eq!(acc.finish().peak_heap_bytes, 900);
    }

    #[test]
    fn stats_env_enables_nix_show_stats_only_when_metrics_on() {
        assert_eq!(
            eval_worker_stats_env(true),
            [
                ("NIX_SHOW_STATS", "1"),
                ("NIX_SHOW_STATS_PATH", "/dev/null")
            ]
            .as_slice()
        );
        assert!(eval_worker_stats_env(false).is_empty());
    }
}
