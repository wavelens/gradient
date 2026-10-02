/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recording is sync and never touching the database. Storage, wire and passthrough hot paths can
//! call it. `gradient-db` is flushing it into `metric_rollup`.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sync::Mutex;

pub mod metric {
    pub const STORAGE_OP_MS: &str = "storage.op_ms";
    pub const STORAGE_OP_ERRORS: &str = "storage.op_errors";
    pub const PROTO_BULK_LANE_FILL: &str = "proto.bulk_lane_fill";
    pub const PROTO_CONTROL_LANE_FILL: &str = "proto.control_lane_fill";
    pub const PROTO_SEND_STALLS: &str = "proto.send_stalls";
    pub const NAR_SERVES_WAITING: &str = "nar.serves_waiting";
    pub const NAR_SERVES_ACTIVE: &str = "nar.serves_active";
    pub const NAR_SERVE_FAILURES: &str = "nar.serve_failures";
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Agg {
    pub count: i64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub sum_sq: f64,
}

impl Agg {
    fn of(value: f64) -> Self {
        Self {
            count: 1,
            sum: value,
            min: value,
            max: value,
            sum_sq: value * value,
        }
    }

    fn add(&mut self, value: f64) {
        self.count += 1;
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.sum_sq += value * value;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub metric: &'static str,
    pub label: String,
    pub minute: i64,
}

#[derive(Default)]
pub struct MinuteStats {
    buckets: Mutex<HashMap<Key, Agg>>,
}

impl MinuteStats {
    pub fn record(&self, metric: &'static str, label: impl Into<String>, value: f64) {
        self.record_at(metric, label, value, current_minute());
    }

    pub fn record_at(
        &self,
        metric: &'static str,
        label: impl Into<String>,
        value: f64,
        minute: i64,
    ) {
        let key = Key {
            metric,
            label: label.into(),
            minute,
        };

        self.buckets
            .lock()
            .entry(key)
            .and_modify(|agg| agg.add(value))
            .or_insert_with(|| Agg::of(value));
    }

    pub fn take(&self) -> Vec<(Key, Agg)> {
        self.buckets.lock().drain().collect()
    }

    pub fn snapshot(&self) -> Vec<(Key, Agg)> {
        self.buckets
            .lock()
            .iter()
            .map(|(k, a)| (k.clone(), *a))
            .collect()
    }

    pub fn sample(&self, gauges: &Gauges) {
        let minute = current_minute();
        let permille = |p: &Peak| f64::from(p.take()) / 1000.0;

        self.record_at(
            metric::PROTO_BULK_LANE_FILL,
            "",
            permille(&gauges.bulk_lane_peak),
            minute,
        );

        self.record_at(
            metric::PROTO_CONTROL_LANE_FILL,
            "",
            permille(&gauges.control_lane_peak),
            minute,
        );

        self.record_at(
            metric::NAR_SERVES_WAITING,
            "",
            gauges.serves_waiting.get() as f64,
            minute,
        );

        self.record_at(
            metric::NAR_SERVES_ACTIVE,
            "",
            gauges.serves_active.get() as f64,
            minute,
        );
    }
}

pub struct Peak(AtomicU32);

impl Peak {
    pub const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn observe(&self, permille: u32) {
        if self.0.load(Ordering::Relaxed) < permille {
            self.0.fetch_max(permille, Ordering::Relaxed);
        }
    }

    pub fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn take(&self) -> u32 {
        self.0.swap(0, Ordering::Relaxed)
    }
}

impl Default for Peak {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Level(AtomicI64);

impl Level {
    pub const fn new() -> Self {
        Self(AtomicI64::new(0))
    }

    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Gauges {
    pub bulk_lane_peak: Peak,
    pub control_lane_peak: Peak,
    pub serves_waiting: Level,
    pub serves_active: Level,
}

impl Gauges {
    pub const fn new() -> Self {
        Self {
            bulk_lane_peak: Peak::new(),
            control_lane_peak: Peak::new(),
            serves_waiting: Level::new(),
            serves_active: Level::new(),
        }
    }
}

impl Default for Gauges {
    fn default() -> Self {
        Self::new()
    }
}

pub static STATS: LazyLock<MinuteStats> = LazyLock::new(MinuteStats::default);
pub static GAUGES: Gauges = Gauges::new();

pub fn current_minute() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    secs - secs % 60
}

pub fn fill_permille(free: usize, max: usize) -> u32 {
    if max == 0 {
        return 0;
    }

    (max.saturating_sub(free) * 1000 / max) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_in_one_minute_merge() {
        let stats = MinuteStats::default();
        stats.record_at(metric::STORAGE_OP_MS, "get", 10.0, 60);
        stats.record_at(metric::STORAGE_OP_MS, "get", 30.0, 60);

        let taken = stats.take();
        assert_eq!(taken.len(), 1);
        let (key, agg) = &taken[0];
        assert_eq!(key.label, "get");
        assert_eq!(agg.count, 2);
        assert_eq!(agg.sum, 40.0);
        assert_eq!(agg.min, 10.0);
        assert_eq!(agg.max, 30.0);
        assert_eq!(agg.sum_sq, 1000.0);
        assert!(stats.take().is_empty());
    }

    #[test]
    fn minutes_and_labels_are_separate_rows() {
        let stats = MinuteStats::default();
        stats.record_at(metric::STORAGE_OP_MS, "get", 1.0, 60);
        stats.record_at(metric::STORAGE_OP_MS, "get", 1.0, 120);
        stats.record_at(metric::STORAGE_OP_MS, "put", 1.0, 60);

        assert_eq!(stats.take().len(), 3);
    }

    #[test]
    fn snapshot_does_not_drain() {
        let stats = MinuteStats::default();
        stats.record_at(metric::PROTO_SEND_STALLS, "bulk", 1.0, 60);

        assert_eq!(stats.snapshot().len(), 1);
        assert_eq!(stats.take().len(), 1);
    }

    #[test]
    fn current_minute_is_minute_aligned() {
        assert_eq!(current_minute() % 60, 0);
    }

    #[test]
    fn peak_keeps_the_highest_until_taken() {
        let peak = Peak::new();
        peak.observe(200);
        peak.observe(900);
        peak.observe(100);

        assert_eq!(peak.take(), 900);
        assert_eq!(peak.get(), 0);
    }

    #[test]
    fn fill_permille_is_the_used_share() {
        assert_eq!(fill_permille(16, 16), 0);
        assert_eq!(fill_permille(0, 16), 1000);
        assert_eq!(fill_permille(4, 16), 750);
        assert_eq!(fill_permille(0, 0), 0);
    }

    #[test]
    fn sample_records_every_gauge_and_resets_peaks() {
        let stats = MinuteStats::default();
        let gauges = Gauges::new();
        gauges.bulk_lane_peak.observe(500);
        gauges.serves_waiting.inc();
        gauges.serves_waiting.inc();
        gauges.serves_active.inc();

        stats.sample(&gauges);

        let taken = stats.take();
        let value = |m: &str| {
            taken
                .iter()
                .find(|(k, _)| k.metric == m)
                .map(|(_, a)| a.sum)
                .expect(m)
        };

        assert_eq!(value(metric::PROTO_BULK_LANE_FILL), 0.5);
        assert_eq!(value(metric::PROTO_CONTROL_LANE_FILL), 0.0);
        assert_eq!(value(metric::NAR_SERVES_WAITING), 2.0);
        assert_eq!(value(metric::NAR_SERVES_ACTIVE), 1.0);
        assert_eq!(gauges.bulk_lane_peak.get(), 0);
        assert_eq!(gauges.serves_waiting.get(), 2, "levels are not reset");
    }
}
