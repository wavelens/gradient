/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use gradient_entity::eval_cost_report::{
    Column as CCostReport, Entity as ECostReport, Model as MCostReport,
};
use gradient_entity::evaluation_metric::{
    Column as CEvaluationMetric, Entity as EEvaluationMetric, Model as MEvaluationMetric,
};
use gradient_entity::ids::{EvalCostReportId, EvaluationId, EvaluationMetricId, TaskId};
use gradient_wire::types::{EvalSetCost, EvalStatsReport, EvalSubtreeCost};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    TransactionSession, TransactionTrait,
};

use crate::eval_costs::record_eval_costs;

pub async fn stage_eval_report<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    job: &str,
    report: &EvalStatsReport,
) -> Result<(), DbErr> {
    let report = serde_json::to_value(report).map_err(|e| DbErr::Json(e.to_string()))?;
    let row = MCostReport {
        id: EvalCostReportId::now_v7(),
        evaluation,
        job: job.to_owned(),
        report,
        created_at: gradient_types::now(),
    };

    ECostReport::insert(row.into_active_model())
        .exec_without_returning(db)
        .await?;

    Ok(())
}

pub async fn fold_eval_reports<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    evaluation: EvaluationId,
    task: Option<TaskId>,
) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    let reports = staged_reports(&txn, evaluation).await?;
    if reports.is_empty() {
        return txn.commit().await;
    }

    let merged = merge_reports(reports);
    upsert_metric(&txn, evaluation, &merged).await?;
    let has_costs = !merged.sets.is_empty() || !merged.subtrees.is_empty();
    if let Some(task) = task
        && has_costs
    {
        record_eval_costs(&txn, task, &merged.sets, &merged.subtrees).await?;
    }

    ECostReport::delete_many()
        .filter(CCostReport::Evaluation.eq(evaluation))
        .exec(&txn)
        .await?;
    txn.commit().await
}

pub fn merge_reports(reports: Vec<EvalStatsReport>) -> EvalStatsReport {
    let mut merged = EvalStatsReport {
        worker_id: reports
            .first()
            .map(|report| report.worker_id.clone())
            .unwrap_or_default(),
        ..Default::default()
    };
    let mut sets: BTreeMap<Vec<String>, Vec<EvalSetCost>> = BTreeMap::new();
    let mut subtrees: BTreeMap<Vec<String>, EvalSubtreeCost> = BTreeMap::new();

    for report in reports {
        add_totals(&mut merged, &report);
        merged.per_entry_point.extend(report.per_entry_point);
        merged.flake_nodes.extend(report.flake_nodes);
        for set in report.sets {
            sets.entry(set.path.clone()).or_default().push(set);
        }

        for subtree in report.subtrees {
            let sum = subtrees
                .entry(subtree.path.clone())
                .or_insert_with(|| EvalSubtreeCost {
                    path: subtree.path.clone(),
                    ..Default::default()
                });
            sum.attrs += subtree.attrs;
            sum.eval_ms += subtree.eval_ms;
        }
    }

    merged.sets = sets.into_values().map(merge_set).collect();
    merged.subtrees = subtrees.into_values().collect();
    merged
}

fn add_totals(merged: &mut EvalStatsReport, report: &EvalStatsReport) {
    merged.total_thunks += report.total_thunks;
    merged.fn_calls += report.fn_calls;
    merged.primop_calls += report.primop_calls;
    merged.lookups += report.lookups;
    merged.alloc_bytes += report.alloc_bytes;
    merged.total_eval_ms += report.total_eval_ms;
    merged.peak_heap_mb = merged.peak_heap_mb.max(report.peak_heap_mb);
    merged.peak_rss_mb = merged.peak_rss_mb.max(report.peak_rss_mb);
}

fn merge_set(reported: Vec<EvalSetCost>) -> EvalSetCost {
    let startups: Vec<u64> = reported.iter().filter_map(|set| set.startup_ms).collect();

    EvalSetCost {
        path: reported[0].path.clone(),
        startup_ms: median(startups),
        peak_rss_mb: reported
            .iter()
            .map(|set| set.peak_rss_mb)
            .max()
            .unwrap_or(0),
    }
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    let middle = values.len() / 2;
    match values.len() {
        0 => None,
        len if len % 2 == 1 => Some(values[middle]),
        _ => Some(values[middle - 1].midpoint(values[middle])),
    }
}

async fn staged_reports(
    db: &impl ConnectionTrait,
    evaluation: EvaluationId,
) -> Result<Vec<EvalStatsReport>, DbErr> {
    ECostReport::find()
        .filter(CCostReport::Evaluation.eq(evaluation))
        .order_by_asc(CCostReport::CreatedAt)
        .order_by_asc(CCostReport::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|row| serde_json::from_value(row.report).map_err(|e| DbErr::Json(e.to_string())))
        .collect()
}

async fn upsert_metric(
    db: &impl ConnectionTrait,
    evaluation: EvaluationId,
    report: &EvalStatsReport,
) -> Result<(), DbErr> {
    let metric = MEvaluationMetric {
        id: EvaluationMetricId::now_v7(),
        evaluation,
        total_thunks: report.total_thunks as i64,
        fn_calls: report.fn_calls as i64,
        primop_calls: report.primop_calls as i64,
        lookups: report.lookups as i64,
        alloc_bytes: report.alloc_bytes as i64,
        peak_heap_mb: report.peak_heap_mb as i64,
        peak_rss_mb: report.peak_rss_mb as i64,
        total_eval_ms: report.total_eval_ms as i64,
        worker_id: report.worker_id.clone(),
        created_at: gradient_types::now(),
        ..Default::default()
    }
    .into_active_model();

    let stats = [
        CEvaluationMetric::TotalThunks,
        CEvaluationMetric::FnCalls,
        CEvaluationMetric::PrimopCalls,
        CEvaluationMetric::Lookups,
        CEvaluationMetric::AllocBytes,
        CEvaluationMetric::PeakHeapMb,
        CEvaluationMetric::PeakRssMb,
        CEvaluationMetric::TotalEvalMs,
        CEvaluationMetric::WorkerId,
    ];
    EEvaluationMetric::insert(metric)
        .on_conflict(
            OnConflict::column(CEvaluationMetric::Evaluation)
                .update_columns(stats)
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
    use sea_orm::{DatabaseConnection, Statement};

    fn path(name: &str) -> Vec<String> {
        vec!["hydraJobs".into(), name.into()]
    }

    fn set(name: &str, startup_ms: Option<u64>, peak_rss_mb: u64) -> EvalSetCost {
        EvalSetCost {
            path: path(name),
            startup_ms,
            peak_rss_mb,
        }
    }

    fn subtree(name: &str, attrs: u64, eval_ms: u64) -> EvalSubtreeCost {
        EvalSubtreeCost {
            path: path(name),
            attrs,
            eval_ms,
        }
    }

    fn member(worker: &str, thunks: u64, peak_rss_mb: u64) -> EvalStatsReport {
        EvalStatsReport {
            worker_id: worker.into(),
            total_thunks: thunks,
            fn_calls: thunks / 2,
            total_eval_ms: thunks / 10,
            peak_heap_mb: peak_rss_mb / 2,
            peak_rss_mb,
            ..Default::default()
        }
    }

    fn staged(evaluation: EvaluationId, report: &EvalStatsReport) -> MCostReport {
        MCostReport {
            id: EvalCostReportId::now_v7(),
            evaluation,
            job: format!("eval:{evaluation}"),
            report: serde_json::to_value(report).expect("a report encodes"),
            created_at: gradient_types::now(),
        }
    }

    fn holding(rows: Vec<MCostReport>) -> DatabaseConnection {
        mock_db()
            .append_query_results([rows])
            .append_exec_results((0..16).map(|_| exec_ok()))
            .into_connection()
    }

    fn starting_with<'a>(statements: &'a [Statement], prefix: &str) -> Vec<&'a Statement> {
        statements
            .iter()
            .filter(|statement| statement.sql.starts_with(prefix))
            .collect()
    }

    fn bound(statement: &Statement) -> String {
        format!("{:?}", statement.values)
    }

    #[test]
    fn counters_add_and_peaks_take_the_maximum() {
        let merged = merge_reports(vec![member("w1", 1000, 800), member("w2", 3000, 500)]);

        assert_eq!(
            (
                merged.total_thunks,
                merged.fn_calls,
                merged.total_eval_ms,
                merged.peak_heap_mb,
                merged.peak_rss_mb,
                merged.worker_id.as_str(),
            ),
            (4000, 2000, 400, 400, 800, "w1")
        );
    }

    #[test]
    fn a_set_reported_by_two_members_takes_the_median_startup() {
        let first = EvalStatsReport {
            sets: vec![set("perl", Some(100), 900), set("python", None, 300)],
            subtrees: vec![subtree("perl", 10, 500)],
            ..Default::default()
        };
        let second = EvalStatsReport {
            sets: vec![set("perl", Some(300), 1200), set("python", None, 400)],
            subtrees: vec![subtree("perl", 5, 100), subtree("python", 2, 40)],
            ..Default::default()
        };

        let merged = merge_reports(vec![first, second]);

        assert_eq!(
            merged.sets,
            vec![set("perl", Some(200), 1200), set("python", None, 400)]
        );
        assert_eq!(
            merged.subtrees,
            vec![subtree("perl", 15, 600), subtree("python", 2, 40)]
        );
    }

    #[tokio::test]
    async fn member_reports_of_one_evaluation_fold_into_one_sample() {
        let evaluation = EvaluationId::now_v7();
        let primary = EvalStatsReport {
            sets: vec![set("perl", Some(100), 900)],
            subtrees: vec![subtree("perl", 10, 500)],
            ..member("w1", 1000, 800)
        };
        let companion = EvalStatsReport {
            sets: vec![set("perl", Some(300), 700)],
            subtrees: vec![subtree("perl", 5, 100)],
            ..member("w2", 2000, 600)
        };
        let db = holding(vec![
            staged(evaluation, &primary),
            staged(evaluation, &companion),
        ]);

        fold_eval_reports(&db, evaluation, Some(TaskId::now_v7()))
            .await
            .expect("fold");

        let statements = raw_statements(db.into_transaction_log());
        let metrics = starting_with(&statements, "INSERT INTO \"evaluation_metric\"");
        assert_eq!(metrics.len(), 1, "{statements:?}");
        assert!(
            bound(metrics[0]).contains("BigInt(Some(3000))"),
            "{}",
            bound(metrics[0])
        );
        assert_eq!(
            starting_with(&statements, "INSERT INTO \"eval_set_cost\"").len(),
            1
        );
        let subtrees = starting_with(&statements, "INSERT INTO \"eval_subtree_cost\"");
        assert_eq!(subtrees.len(), 1);
        assert!(
            bound(subtrees[0]).contains("Double(Some(15.0))"),
            "{}",
            bound(subtrees[0])
        );
        assert_eq!(
            starting_with(&statements, "DELETE FROM \"eval_cost_report\"").len(),
            1
        );
    }

    #[tokio::test]
    async fn a_standalone_evaluation_folds_its_single_report() {
        let evaluation = EvaluationId::now_v7();
        let db = holding(vec![staged(evaluation, &member("w1", 1000, 800))]);

        fold_eval_reports(&db, evaluation, None)
            .await
            .expect("fold");

        let statements = raw_statements(db.into_transaction_log());
        let metrics = starting_with(&statements, "INSERT INTO \"evaluation_metric\"");
        assert_eq!(metrics.len(), 1, "{statements:?}");
        let values = bound(metrics[0]);
        assert!(values.contains("BigInt(Some(1000))"), "{values}");
        assert!(values.contains("\"w1\""), "{values}");
        assert!(starting_with(&statements, "INSERT INTO \"eval_set_cost\"").is_empty());
        assert_eq!(
            starting_with(&statements, "DELETE FROM \"eval_cost_report\"").len(),
            1
        );
    }
}
