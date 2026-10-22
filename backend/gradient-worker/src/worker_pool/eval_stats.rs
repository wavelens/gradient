/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use gradient_eval::pool::CallStats;
pub use gradient_eval::stats::StatsDelta;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cost {
    pub thunks: u64,
    pub fn_calls: u64,
    pub eval_ms: u64,
    pub alloc_bytes: u64,
}

impl Cost {
    fn of(delta: &StatsDelta, eval_ms: u64) -> Self {
        Self {
            thunks: delta.nr_thunks,
            fn_calls: delta.nr_function_calls,
            eval_ms,
            alloc_bytes: delta.alloc_bytes,
        }
    }

    fn add(&mut self, other: Cost) {
        self.thunks += other.thunks;
        self.fn_calls += other.fn_calls;
        self.eval_ms += other.eval_ms;
        self.alloc_bytes += other.alloc_bytes;
    }

    fn without(self, other: Cost) -> Self {
        Self {
            thunks: self.thunks.saturating_sub(other.thunks),
            fn_calls: self.fn_calls.saturating_sub(other.fn_calls),
            eval_ms: self.eval_ms.saturating_sub(other.eval_ms),
            alloc_bytes: self.alloc_bytes.saturating_sub(other.alloc_bytes),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EntryPointCost {
    pub attr: String,
    pub cost: Cost,
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

#[derive(Debug, Default)]
pub(crate) struct EvalStatsAccumulator {
    totals: EvalStatsTotals,
    buckets: HashMap<String, Cost>,
}

impl EvalStatsAccumulator {
    pub fn observe(&mut self, call: CallStats) {
        let Some(delta) = call.stats else {
            return;
        };
        self.add_totals(&delta, call.rss);

        let mut walk = Cost::of(&delta, call.eval_ms);
        for (attr, attr_cost) in call.attrs {
            let cost = Cost::of(&attr_cost.stats, attr_cost.eval_ms);
            walk = walk.without(cost);
            self.charge(attr, cost);
        }
        if walk != Cost::default() {
            self.charge(call.entry_point, walk);
        }
    }

    pub fn finish(mut self) -> EvalStatsTotals {
        self.totals.per_entry_point = self
            .buckets
            .into_iter()
            .map(|(attr, cost)| EntryPointCost { attr, cost })
            .collect();
        self.totals
    }

    fn add_totals(&mut self, delta: &StatsDelta, rss_bytes: u64) {
        self.totals.total_thunks += delta.nr_thunks;
        self.totals.fn_calls += delta.nr_function_calls;
        self.totals.primop_calls += delta.nr_primop_calls;
        self.totals.lookups += delta.nr_lookups;
        self.totals.alloc_bytes += delta.alloc_bytes;
        self.totals.peak_heap_bytes = self.totals.peak_heap_bytes.max(delta.gc_heap_size);
        self.totals.peak_rss_bytes = self.totals.peak_rss_bytes.max(rss_bytes);
    }

    fn charge(&mut self, attr: String, cost: Cost) {
        self.buckets.entry(attr).or_default().add(cost);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_eval::stats::AttrCost;

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

    fn call(entry_point: &str, delta: StatsDelta, rss: u64) -> CallStats {
        CallStats {
            entry_point: entry_point.into(),
            stats: Some(delta),
            eval_ms: 0,
            attrs: vec![],
            rss,
        }
    }

    fn costs(totals: EvalStatsTotals) -> Vec<(String, Cost)> {
        let mut costs: Vec<(String, Cost)> = totals
            .per_entry_point
            .into_iter()
            .map(|c| (c.attr, c.cost))
            .collect();
        costs.sort_by(|x, y| x.0.cmp(&y.0));
        costs
    }

    #[test]
    fn sums_totals_across_requests() {
        let mut acc = EvalStatsAccumulator::default();
        acc.observe(call("packages.x86_64-linux.a", d(10, 2, 100), 500));
        acc.observe(call("packages.x86_64-linux.a", d(5, 1, 50), 700));
        let r = acc.finish();
        assert_eq!(r.total_thunks, 15);
        assert_eq!(r.fn_calls, 3);
        assert_eq!(r.alloc_bytes, 150);
        assert_eq!(r.peak_rss_bytes, 700, "peak RSS is the max observed");
    }

    #[test]
    fn each_attribute_of_a_call_gets_its_own_row_and_the_walk_stays_with_the_pattern() {
        let attr = |name: &str, thunks, eval_ms| {
            (
                name.to_string(),
                AttrCost {
                    stats: d(thunks, 0, 0),
                    eval_ms,
                },
            )
        };
        let mut acc = EvalStatsAccumulator::default();
        acc.observe(CallStats {
            eval_ms: 50,
            attrs: vec![attr("hydraJobs.a", 60, 30), attr("hydraJobs.b", 30, 15)],
            ..call("hydraJobs.*", d(100, 0, 0), 0)
        });
        acc.observe(CallStats {
            eval_ms: 20,
            attrs: vec![attr("hydraJobs.c", 40, 20)],
            ..call("hydraJobs.*", d(40, 0, 0), 0)
        });

        let row = |attr: &str, thunks, eval_ms| {
            (
                attr.to_string(),
                Cost {
                    thunks,
                    eval_ms,
                    ..Default::default()
                },
            )
        };
        assert_eq!(
            costs(acc.finish()),
            vec![
                row("hydraJobs.*", 10, 5),
                row("hydraJobs.a", 60, 30),
                row("hydraJobs.b", 30, 15),
                row("hydraJobs.c", 40, 20),
            ]
        );
    }

    #[test]
    fn heap_peak_is_max_gauge_not_sum() {
        let mut acc = EvalStatsAccumulator::default();
        acc.observe(call(
            "a",
            StatsDelta {
                gc_heap_size: 900,
                ..d(0, 0, 0)
            },
            0,
        ));
        acc.observe(call(
            "a",
            StatsDelta {
                gc_heap_size: 300,
                ..d(0, 0, 0)
            },
            0,
        ));
        assert_eq!(acc.finish().peak_heap_bytes, 900);
    }
}
