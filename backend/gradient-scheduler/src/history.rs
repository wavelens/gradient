/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resource-usage predictions derived from historical `derivation_metric` rows.

use gradient_core::types::{CDerivationMetric, EDerivationMetric, MDerivationMetric};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

/// Most recent rows considered when predicting; bounds query cost.
const HISTORY_WINDOW: u64 = 200;

/// Map a closure size in bytes to a coarse log2-of-megabytes bucket. Builds
/// within ±1 bucket are treated as comparable for prediction purposes.
pub fn closure_bucket(closure_size_bytes: i64) -> i64 {
    let mb = (closure_size_bytes / 1_048_576).max(1);
    (mb as f64).log2().floor() as i64
}

/// Inclusive byte bounds covering buckets `[bucket-1, bucket+1]`.
fn bucket_bounds(closure_size_bytes: i64) -> (i64, i64) {
    let bucket = closure_bucket(closure_size_bytes);
    let lo_bucket = (bucket - 1).max(0);
    let hi_bucket = bucket + 1;
    let lo = if lo_bucket == 0 { 0 } else { (1i64 << lo_bucket) * 1_048_576 };
    let hi = ((1i64 << (hi_bucket + 1)) * 1_048_576) - 1;
    (lo, hi)
}

/// Predict resource usage for a build from past metrics of the same `pname`,
/// optionally narrowed to comparable closure sizes (±1 bucket). Returns the
/// default (zero samples) prediction when no history exists.
pub async fn predict(
    db: &impl ConnectionTrait,
    pname: &str,
    closure_size: Option<i64>,
) -> gradient_score::HistoryPrediction {
    let mut query = EDerivationMetric::find().filter(CDerivationMetric::Pname.eq(pname));
    if let Some(size) = closure_size {
        let (lo, hi) = bucket_bounds(size);
        query = query
            .filter(CDerivationMetric::ClosureSize.gte(lo))
            .filter(CDerivationMetric::ClosureSize.lte(hi));
    }

    let rows = match query
        .order_by_desc(CDerivationMetric::CreatedAt)
        .limit(HISTORY_WINDOW)
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(_) => return gradient_score::HistoryPrediction::default(),
    };

    summarize(&rows)
}

fn summarize(rows: &[MDerivationMetric]) -> gradient_score::HistoryPrediction {
    if rows.is_empty() {
        return gradient_score::HistoryPrediction::default();
    }

    let samples = rows.len() as u32;

    let mut peaks: Vec<i64> = rows.iter().filter_map(|r| r.peak_ram_mb).collect();
    let predicted_peak_ram_mb = percentile_or_max(&mut peaks, 0.95).max(0) as u64;

    let cpu: Vec<i64> = rows.iter().filter_map(|r| r.cpu_time_ms).collect();
    let avg_cpu_time_ms = mean_nonnull(&cpu);

    let durations: Vec<i64> = rows.iter().filter_map(|r| r.build_time_ms).collect();
    let build_time_ms = mean_nonnull(&durations);

    let disk: Vec<i64> = rows
        .iter()
        .map(|r| r.disk_read_bytes.unwrap_or(0) + r.disk_write_bytes.unwrap_or(0))
        .filter(|&b| b > 0)
        .collect();
    let avg_disk_bytes = if disk.is_empty() {
        0
    } else {
        (disk.iter().sum::<i64>() / disk.len() as i64).max(0) as u64
    };

    let oom = rows.iter().filter(|r| r.oom_killed).count();
    let oom_rate = oom as f32 / samples as f32;

    gradient_score::HistoryPrediction {
        predicted_peak_ram_mb,
        avg_cpu_time_ms,
        build_time_ms,
        avg_disk_bytes,
        oom_rate,
        samples,
    }
}

/// Integer mean of already-collected non-null values, clamped to 0. Empty → 0.
fn mean_nonnull(vals: &[i64]) -> u64 {
    if vals.is_empty() {
        return 0;
    }

    (vals.iter().sum::<i64>() / vals.len() as i64).max(0) as u64
}

/// p95 of the values, falling back to the max when the sample is too small for
/// a meaningful percentile. Returns 0 for an empty set.
fn percentile_or_max(values: &mut [i64], p: f64) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    if values.len() < 20 {
        return *values.last().unwrap();
    }
    let idx = ((values.len() as f64 - 1.0) * p).round() as usize;
    values[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_log2_of_mb() {
        assert_eq!(closure_bucket(1_048_576), 0);
        assert_eq!(closure_bucket(4 * 1_048_576), 2);
        assert_eq!(closure_bucket(1000 * 1_048_576), 9);
    }

    fn metric(peak: Option<i64>, cpu: Option<i64>, oom: bool) -> MDerivationMetric {
        MDerivationMetric {
            peak_ram_mb: peak,
            cpu_time_ms: cpu,
            oom_killed: oom,
            disk_read_bytes: Some(10_000_000),
            disk_write_bytes: Some(40_000_000),
            ..Default::default()
        }
    }

    #[test]
    fn empty_rows_yield_default() {
        let p = summarize(&[]);
        assert_eq!(p.samples, 0);
        assert_eq!(p.predicted_peak_ram_mb, 0);
    }

    #[test]
    fn summarize_aggregates_peak_cpu_and_oom() {
        let rows = vec![
            metric(Some(100), Some(1000), false),
            metric(Some(300), Some(3000), true),
            metric(None, None, false),
        ];
        let p = summarize(&rows);
        assert_eq!(p.samples, 3);
        // Few samples -> max of peaks.
        assert_eq!(p.predicted_peak_ram_mb, 300);
        // Mean of non-null cpu times.
        assert_eq!(p.avg_cpu_time_ms, 2000);
        // 1 of 3 rows OOM-killed.
        assert!((p.oom_rate - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn summarize_aggregates_disk_bytes() {
        let rows = vec![metric(Some(100), Some(1000), false), metric(Some(200), Some(2000), false)];
        let p = summarize(&rows);
        // Mean of (read + write) bytes per row: 10M + 40M = 50M.
        assert_eq!(p.avg_disk_bytes, 50_000_000);
    }

    #[test]
    fn bucket_bounds_widen_by_one_bucket_each_side() {
        let (lo, hi) = bucket_bounds(4 * 1_048_576);
        assert!(lo <= 2 * 1_048_576);
        assert!(hi >= 8 * 1_048_576);
    }

    #[test]
    fn bucket_bounds_bucket0_lower_bound_is_zero() {
        // Bucket 0 covers closures < 2 MiB; lo_bucket clamps to 0, so the
        // lower bound must be 0 (not 1 MiB) to include sub-1-MiB metrics.
        let (lo, _hi) = bucket_bounds(1_048_576);
        assert_eq!(lo, 0);
    }
}
