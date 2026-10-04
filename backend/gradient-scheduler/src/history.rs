/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_types::ids::DerivationId;
use gradient_types::{
    CDerivationMetric, CDerivationOutput, EDerivationMetric, EDerivationOutput, MDerivationMetric,
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

/// Older builds are drifting with toolchain and host changes.
const HISTORY_WINDOW: u64 = 20;

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

    gradient_pool::score::HistoryPrediction {
        output_nar_size: mean_output_nar_size(&output_nar_sizes(db, &rows).await),
        ..summarize(&rows)
    }
}

async fn output_nar_sizes(
    db: &impl ConnectionTrait,
    rows: &[MDerivationMetric],
) -> Vec<(DerivationId, Option<i64>)> {
    let derivations: HashSet<DerivationId> = rows.iter().map(|r| r.derivation).collect();
    if derivations.is_empty() {
        return Vec::new();
    }

    EDerivationOutput::find()
        .select_only()
        .columns([CDerivationOutput::Derivation, CDerivationOutput::NarSize])
        .filter(CDerivationOutput::Derivation.is_in(derivations))
        .into_tuple()
        .all(db)
        .await
        .unwrap_or_default()
}

fn mean_output_nar_size(outputs: &[(DerivationId, Option<i64>)]) -> Option<u64> {
    let mut per_derivation: HashMap<DerivationId, i64> = HashMap::new();
    for (derivation, size) in outputs {
        if let Some(size) = size {
            *per_derivation.entry(*derivation).or_default() += size;
        }
    }

    mean(&per_derivation.into_values().collect::<Vec<_>>())
}

fn summarize(rows: &[MDerivationMetric]) -> gradient_pool::score::HistoryPrediction {
    if rows.is_empty() {
        return gradient_pool::score::HistoryPrediction::default();
    }

    let samples = rows.len() as u32;

    let mut peaks: Vec<i64> = rows.iter().filter_map(|r| r.peak_ram_mb).collect();
    let predicted_peak_ram_mb = percentile_or_max(&mut peaks, 0.95);

    let cpu: Vec<i64> = rows.iter().filter_map(|r| r.cpu_time_ms).collect();
    let avg_cpu_time_ms = mean(&cpu);

    let durations: Vec<i64> = rows.iter().filter_map(|r| r.build_time_ms).collect();
    let build_time_ms = mean(&durations);
    let uncontended: Vec<i64> = rows.iter().filter_map(uncontended_build_time_ms).collect();
    let built_on: Vec<i64> = rows
        .iter()
        .filter(|r| r.build_time_ms.is_some())
        .filter_map(|r| r.cpu_core_score.map(i64::from))
        .collect();

    let disk: Vec<i64> = rows
        .iter()
        .map(|r| r.disk_read_bytes.unwrap_or(0) + r.disk_write_bytes.unwrap_or(0))
        .filter(|&b| b > 0)
        .collect();
    let avg_disk_bytes = mean(&disk);

    let oom = rows.iter().filter(|r| r.oom_killed).count();
    let oom_rate = oom as f32 / samples as f32;

    gradient_pool::score::HistoryPrediction {
        predicted_peak_ram_mb,
        avg_cpu_time_ms,
        build_time_ms,
        uncontended_build_time_ms: mean(&uncontended),
        build_core_score: mean(&built_on).map(|score| score as u32),
        avg_disk_bytes,
        output_nar_size: None,
        oom_rate,
        samples,
    }
}

fn uncontended_build_time_ms(row: &MDerivationMetric) -> Option<i64> {
    let others = row.concurrent_builds.unwrap_or(0).max(0) as u32;
    row.build_time_ms
        .map(|ms| (ms as f64 / gradient_pool::score::contention_factor(others)).round() as i64)
}

fn mean(vals: &[i64]) -> Option<u64> {
    if vals.is_empty() {
        return None;
    }

    Some((vals.iter().sum::<i64>() / vals.len() as i64).max(0) as u64)
}

fn percentile_or_max(values: &mut [i64], p: f64) -> Option<u64> {
    values.sort_unstable();
    let idx = if values.len() < 20 {
        values.len().checked_sub(1)?
    } else {
        ((values.len() as f64 - 1.0) * p).round() as usize
    };
    Some(values[idx].max(0) as u64)
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
        assert_eq!(p.predicted_peak_ram_mb, None);
    }

    #[test]
    fn a_value_no_build_measured_stays_unknown_instead_of_zero() {
        let unmeasured = MDerivationMetric {
            build_time_ms: Some(300),
            ..Default::default()
        };
        let p = summarize(&[unmeasured.clone(), unmeasured]);
        assert_eq!(p.samples, 2);
        assert_eq!(p.build_time_ms, Some(300));
        assert_eq!(p.predicted_peak_ram_mb, None);
        assert_eq!(p.avg_cpu_time_ms, None);
        assert_eq!(p.avg_disk_bytes, None);
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
        assert_eq!(p.predicted_peak_ram_mb, Some(300));
        assert_eq!(p.avg_cpu_time_ms, Some(2000));
        assert!((p.oom_rate - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn the_build_time_drops_the_slowdown_of_the_builds_beside_it() {
        let row = |build_time_ms, concurrent_builds, cpu_core_score| MDerivationMetric {
            build_time_ms: Some(build_time_ms),
            concurrent_builds,
            cpu_core_score,
            ..Default::default()
        };
        let p = summarize(&[
            row(120_000, Some(5), Some(2_000)),
            row(100_000, None, Some(4_000)),
            row(80_000, Some(0), None),
        ]);

        assert_eq!(p.build_time_ms, Some(100_000));
        assert_eq!(p.uncontended_build_time_ms, Some(93_333));
        assert_eq!(p.build_core_score, Some(3_000));
    }

    #[test]
    fn summarize_aggregates_disk_bytes() {
        let rows = vec![
            metric(Some(100), Some(1000), false),
            metric(Some(200), Some(2000), false),
        ];
        let p = summarize(&rows);
        assert_eq!(p.avg_disk_bytes, Some(50_000_000));
    }

    #[test]
    fn the_output_size_is_the_mean_over_builds_of_their_summed_outputs() {
        let (one, two) = (DerivationId::now_v7(), DerivationId::now_v7());
        let outputs = [
            (one, Some(100)),
            (one, Some(300)),
            (two, Some(200)),
            (two, None),
        ];
        assert_eq!(mean_output_nar_size(&outputs), Some(300));
        assert_eq!(mean_output_nar_size(&[(one, None)]), None);
    }

    #[tokio::test]
    async fn predict_reads_the_latest_rows_of_the_same_pname_and_architecture() {
        let derivation = DerivationId::now_v7();
        let output: std::collections::BTreeMap<String, sea_orm::Value> = [
            ("derivation".to_owned(), derivation.into_inner().into()),
            ("nar_size".to_owned(), 4_096_i64.into()),
        ]
        .into_iter()
        .collect();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![MDerivationMetric {
                derivation,
                ..metric(Some(100), Some(1000), false)
            }]])
            .append_query_results([vec![output]])
            .into_connection();

        let p = predict(&db, "hello", "x86_64-linux").await;
        assert_eq!(p.samples, 1);
        assert_eq!(p.output_nar_size, Some(4_096));

        let sql: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(|stmt| stmt.sql.clone())
            .collect();
        let [sql, outputs_sql] = sql.as_slice() else {
            panic!("two statements expected: {sql:?}")
        };
        assert!(
            outputs_sql.contains(r#""derivation_output"."derivation" IN ($1)"#),
            "{outputs_sql}"
        );
        assert!(sql.contains(r#""pname" = $1"#), "{sql}");
        assert!(sql.contains(r#""architecture" = $2"#), "{sql}");
        assert!(!sql.contains(r#""closure_size" >="#), "{sql}");
        assert!(
            sql.contains(r#"ORDER BY "derivation_metric"."created_at" DESC"#),
            "{sql}"
        );
    }
}
