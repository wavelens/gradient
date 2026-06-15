/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Persists worker-reported `EvalStatsReport` into the eval-metric tables.

use sea_orm::{ActiveModelTrait, IntoActiveModel};
use tracing::{debug, warn};

use gradient_types::proto::EvalStatsReport;
use gradient_types::*;

use crate::Scheduler;

impl Scheduler {
    pub async fn record_eval_metrics(
        &self,
        job_id: &str,
        report: EvalStatsReport,
    ) -> anyhow::Result<()> {
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(j) => j.evaluation_id(),
                None => {
                    debug!(%job_id, "EvalStats dropped: no active job");
                    return Ok(());
                }
            }
        };

        for cost in report.per_entry_point {
            let row = MEvaluationAttrCost {
                id: EvaluationAttrCostId::now_v7(),
                evaluation: evaluation_id,
                attr: cost.attr,
                thunks: cost.thunks as i64,
                fn_calls: cost.fn_calls as i64,
                eval_ms: cost.eval_ms as i64,
                alloc_bytes: cost.alloc_bytes as i64,
            }
            .into_active_model();

            if let Err(e) = row.insert(&self.state.worker_db).await {
                warn!(%evaluation_id, error = %e, "failed to record evaluation_attr_cost");
            }
        }

        for node in report.flake_nodes {
            let row = MFlakeOutputNode {
                id: FlakeOutputNodeId::now_v7(),
                evaluation: evaluation_id,
                path: node.path,
                parent: node.parent,
                name: node.name,
                kind: node.kind,
                is_derivation: node.is_derivation,
                drv_path: node.drv_path,
            }
            .into_active_model();

            if let Err(e) = row.insert(&self.state.worker_db).await {
                warn!(%evaluation_id, error = %e, "failed to record flake_output_node");
            }
        }

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
            fetch_ms: report.fetch_ms as i64,
            eval_flake_ms: report.eval_flake_ms as i64,
            eval_drv_ms: report.eval_drv_ms as i64,
            total_eval_ms: report.total_eval_ms as i64,
            worker_id: report.worker_id,
            created_at: gradient_types::now(),
        }
        .into_active_model();

        if let Err(e) = metric.insert(&self.state.worker_db).await {
            warn!(%evaluation_id, error = %e, "failed to record evaluation_metric");
        }

        Ok(())
    }
}
