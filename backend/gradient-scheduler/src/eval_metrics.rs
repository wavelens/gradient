/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::chunked::insert_in_chunks;
use gradient_db::eval_costs::ROWS_PER_STATEMENT;
use gradient_db::eval_reports::{fold_eval_reports, stage_eval_report};
use gradient_db::pool::WorkerDb;
use gradient_wire::types::{EvalAttrCost, EvalStatsReport, FlakeOutputNode};
use sea_orm::IntoActiveModel;
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
        report.worker_id = worker_id.to_owned();
        if let Err(e) = stage_eval_report(db, evaluation_id, job_id, &report).await {
            warn!(%evaluation_id, error = %e, "failed to stage eval stats");
        }

        Ok(())
    }

    pub(crate) async fn fold_eval_stats(&self, evaluation: EvaluationId, task: Option<TaskId>) {
        if let Err(e) = fold_eval_reports(&self.db.worker_db, evaluation, task).await {
            warn!(%evaluation, error = %e, "failed to fold eval stats");
        }
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
