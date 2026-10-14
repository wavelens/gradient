/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation_metric::{
    Column as CEvaluationMetric, Entity as EEvaluationMetric,
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel};
use tracing::{debug, warn};

use gradient_types::*;
use gradient_wire::types::EvalStatsReport;

use crate::Scheduler;

impl Scheduler {
    pub async fn record_eval_metrics(
        &self,
        job_id: &str,
        report: EvalStatsReport,
    ) -> anyhow::Result<()> {
        let Some(evaluation_id) = self.active_job(job_id).await.map(|j| j.evaluation_id()) else {
            debug!(%job_id, "EvalStats dropped: no active job");
            return Ok(());
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

            if let Err(e) = row.insert(&self.db.worker_db).await {
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

            if let Err(e) = row.insert(&self.db.worker_db).await {
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
            total_eval_ms: report.total_eval_ms as i64,
            worker_id: report.worker_id,
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
            .exec(&self.db.worker_db)
            .await
        {
            warn!(%evaluation_id, error = %e, "failed to record evaluation_metric");
        }

        Ok(())
    }
}
