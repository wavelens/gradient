/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::chunked::insert_in_chunks;
use gradient_db::eval_costs::{ROWS_PER_STATEMENT, record_eval_costs};
use gradient_db::pool::WorkerDb;
use gradient_entity::evaluation_metric::{
    Column as CEvaluationMetric, Entity as EEvaluationMetric,
};
use gradient_wire::types::{EvalAttrCost, EvalStatsReport, FlakeOutputNode};
use sea_orm::sea_query::OnConflict;
use sea_orm::{EntityTrait, IntoActiveModel};
use tracing::{debug, warn};

use gradient_types::*;

use crate::Scheduler;

impl Scheduler {
    pub async fn record_eval_metrics(
        &self,
        worker_id: &str,
        job_id: &str,
        mut report: EvalStatsReport,
    ) -> anyhow::Result<()> {
        let Some(evaluation_id) = self.active_job(job_id).await.map(|j| j.evaluation_id()) else {
            debug!(%job_id, "EvalStats dropped: no active job");
            return Ok(());
        };

        let db = &self.db.worker_db;
        let costs = std::mem::take(&mut report.per_entry_point);
        record_attr_costs(db, evaluation_id, costs).await;
        let nodes = std::mem::take(&mut report.flake_nodes);
        record_flake_nodes(db, evaluation_id, nodes).await;
        record_task_estimates(db, evaluation_id, &report).await;
        record_metric(db, evaluation_id, worker_id, &report).await;

        Ok(())
    }
}

async fn record_attr_costs(db: &WorkerDb, evaluation_id: EvaluationId, costs: Vec<EvalAttrCost>) {
    let rows = costs
        .into_iter()
        .map(|cost| {
            MEvaluationAttrCost {
                id: EvaluationAttrCostId::now_v7(),
                evaluation: evaluation_id,
                attr: cost.attr,
                thunks: cost.thunks as i64,
                fn_calls: cost.fn_calls as i64,
                eval_ms: cost.eval_ms as i64,
                alloc_bytes: cost.alloc_bytes as i64,
            }
            .into_active_model()
        })
        .collect();

    if let Err(e) = insert_in_chunks(db, rows, ROWS_PER_STATEMENT, None).await {
        warn!(%evaluation_id, error = %e, "failed to record evaluation_attr_cost");
    }
}

async fn record_flake_nodes(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    nodes: Vec<FlakeOutputNode>,
) {
    let rows = nodes
        .into_iter()
        .map(|node| {
            MFlakeOutputNode {
                id: FlakeOutputNodeId::now_v7(),
                evaluation: evaluation_id,
                path: node.path,
                parent: node.parent,
                name: node.name,
                kind: node.kind,
                is_derivation: node.is_derivation,
                drv_path: node.drv_path,
            }
            .into_active_model()
        })
        .collect();

    if let Err(e) = insert_in_chunks(db, rows, ROWS_PER_STATEMENT, None).await {
        warn!(%evaluation_id, error = %e, "failed to record flake_output_node");
    }
}

async fn record_task_estimates(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    report: &EvalStatsReport,
) {
    if report.sets.is_empty() && report.subtrees.is_empty() {
        return;
    }

    let recorded = match gradient_db::evaluations::task_of(db, evaluation_id).await {
        Ok(Some(task)) => record_eval_costs(db, task, &report.sets, &report.subtrees).await,
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };

    if let Err(e) = recorded {
        warn!(%evaluation_id, error = %e, "failed to record eval cost estimates");
    }
}

async fn record_metric(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    worker_id: &str,
    report: &EvalStatsReport,
) {
    let metric = MEvaluationMetric {
        id: EvaluationMetricId::now_v7(),
        evaluation: evaluation_id,
        total_thunks: report.total_thunks as i64,
        fn_calls: report.fn_calls as i64,
        primop_calls: report.primop_calls as i64,
        lookups: report.lookups as i64,
        alloc_bytes: report.alloc_bytes as i64,
        peak_heap_mb: report.peak_heap_mb as i64,
        peak_rss_mb: report.peak_rss_mb as i64,
        total_eval_ms: report.total_eval_ms as i64,
        worker_id: worker_id.to_owned(),
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
    if let Err(e) = EEvaluationMetric::insert(metric)
        .on_conflict(
            OnConflict::column(CEvaluationMetric::Evaluation)
                .update_columns(stats)
                .to_owned(),
        )
        .exec(db)
        .await
    {
        warn!(%evaluation_id, error = %e, "failed to record evaluation_metric");
    }
}
