/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resource-usage predictions derived from historical `derivation_metric` rows.

use gradient_types::{CDerivationMetric, EDerivationMetric, MDerivationMetric};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

/// Most recent rows considered: older builds drift with toolchain and host changes.
const HISTORY_WINDOW: u64 = 20;

/// Predict resource usage for a build from the latest metrics of the same
/// `history_name` on the same architecture. Returns the default (zero samples)
/// prediction when no history exists.
pub async fn predict(
    db: &impl ConnectionTrait,
    history_name: &str,
    architecture: &str,
) -> gradient_pool::score::HistoryPrediction {
    let rows = match EDerivationMetric::find()
        .filter(CDerivationMetric::Pname.eq(history_name))
        .filter(CDerivationMetric::Architecture.eq(architecture))
        .order_by_desc(CDerivationMetric::CreatedAt)
        .limit(HISTORY_WINDOW)
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(_) => return gradient_pool::score::HistoryPrediction::default(),
    };

    summarize(&rows)
}

fn summarize(rows: &[MDerivationMetric]) -> gradient_pool::score::HistoryPrediction {
    if rows.is_empty() {
        return gradient_pool::score::HistoryPrediction::default();
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

    gradient_pool::score::HistoryPrediction {
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
        return values[values.len() - 1];
    }
    let idx = ((values.len() as f64 - 1.0) * p).round() as usize;
    values[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let rows = vec![
            metric(Some(100), Some(1000), false),
            metric(Some(200), Some(2000), false),
        ];
        let p = summarize(&rows);
        // Mean of (read + write) bytes per row: 10M + 40M = 50M.
        assert_eq!(p.avg_disk_bytes, 50_000_000);
    }

    #[tokio::test]
    async fn predict_reads_the_latest_rows_of_the_same_pname_and_architecture() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![metric(Some(100), Some(1000), false)]])
            .into_connection();

        let p = predict(&db, "hello", "x86_64-linux").await;
        assert_eq!(p.samples, 1);

        let sql: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(|stmt| stmt.sql.clone())
            .collect();
        let [sql] = sql.as_slice() else {
            panic!("one statement expected: {sql:?}")
        };
        assert!(sql.contains(r#""pname" = $1"#), "{sql}");
        assert!(sql.contains(r#""architecture" = $2"#), "{sql}");
        assert!(!sql.contains(r#""closure_size" >="#), "{sql}");
        assert!(
            sql.contains(r#"ORDER BY "derivation_metric"."created_at" DESC"#),
            "{sql}"
        );
    }
}
