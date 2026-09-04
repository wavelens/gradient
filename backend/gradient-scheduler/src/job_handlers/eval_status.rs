/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Eval status transitions, result handling, and message recording.

use anyhow::Result;
use sea_orm::EntityTrait;
use tracing::{debug, warn};

use gradient_exec::strip_nix_store_prefix;
use gradient_graph::IngestBatch;
use gradient_types::proto::DiscoveredDerivation;
use gradient_types::*;

use crate::Scheduler;
use crate::eval;
use crate::jobs::PendingJob;

impl Scheduler {
    // ── Eval status transitions ───────────────────────────────────────────────

    pub async fn handle_eval_status_update(
        &self,
        job_id: &str,
        new_status: gradient_entity::evaluation::EvaluationStatus,
    ) {
        let Some(PendingJob::Eval(j)) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = j.evaluation_id;
        match EEvaluation::find_by_id(evaluation_id)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(eval)) => {
                gradient_db::update_evaluation_status(&self.state.db(), eval, new_status).await;
            }
            Ok(None) => warn!(%evaluation_id, "evaluation not found for status update"),
            Err(e) => {
                warn!(error = %e, %evaluation_id, "failed to fetch evaluation for status update")
            }
        }
    }

    /// Persist the archived flake store path on the evaluation row so
    /// follow-up eval-only jobs can dispatch with `FlakeSource::Cached`.
    pub async fn persist_flake_source(&self, job_id: &str, flake_source: Option<String>) {
        use sea_orm::ActiveModelTrait;
        use sea_orm::Set;

        let Some(path) = flake_source else { return };
        let Some(PendingJob::Eval(j)) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = j.evaluation_id;
        let am = gradient_entity::evaluation::ActiveModel {
            id: Set(evaluation_id),
            flake_source: Set(Some(path)),
            ..Default::default()
        };
        if let Err(e) = am.update(&self.state.worker_db).await {
            warn!(error = %e, %evaluation_id, "failed to persist flake_source");
        }
    }

    /// Store the worker-produced candidate lock + bumps on the `input_update`
    /// sidecar so the `OpenPr` action can read them once the verify gate clears.
    pub async fn persist_input_update_result(
        &self,
        job_id: &str,
        candidate_lock: String,
        bumped: Vec<gradient_types::proto::BumpedInputWire>,
    ) {
        use gradient_entity::evaluation_input_update as eiu;
        use sea_orm::{
            ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
        };

        let Some(PendingJob::Eval(j)) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = j.evaluation_id;

        let bumped_json = serde_json::json!(
            bumped
                .iter()
                .map(|b| serde_json::json!({
                    "name": b.name,
                    "old_rev": b.old_rev,
                    "new_rev": b.new_rev,
                }))
                .collect::<Vec<_>>()
        );

        let sidecar = match eiu::Entity::find()
            .filter(eiu::Column::Evaluation.eq(evaluation_id))
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!(%evaluation_id, "input_update sidecar missing for result");
                return;
            }
            Err(e) => {
                warn!(error = %e, %evaluation_id, "loading input_update sidecar");
                return;
            }
        };

        let mut am = sidecar.into_active_model();
        am.candidate_lock = Set(Some(candidate_lock));
        am.bumped_inputs = Set(Some(bumped_json));
        am.updated_at = Set(gradient_types::now());
        if let Err(e) = am.update(&self.state.worker_db).await {
            warn!(error = %e, %evaluation_id, "failed to persist input_update result");
        }
    }

    /// On a discovery `input_update` report, fan the matched inputs out into one
    /// per-input update eval each via the ci trigger helper.
    pub async fn persist_input_update_expansion(&self, job_id: &str, matched: Vec<String>) {
        use sea_orm::{ColumnTrait, QueryFilter};

        let Some(PendingJob::Eval(j)) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = j.evaluation_id;
        let db = &self.state.worker_db;

        let eval = match EEvaluation::find_by_id(evaluation_id).one(db).await {
            Ok(Some(e)) => e,
            _ => {
                warn!(%evaluation_id, "input_update expansion: evaluation missing");
                return;
            }
        };
        let Some(task_id) = eval.task else {
            return;
        };
        let sidecar = match gradient_entity::evaluation_input_update::Entity::find()
            .filter(gradient_entity::evaluation_input_update::Column::Evaluation.eq(evaluation_id))
            .one(db)
            .await
        {
            Ok(Some(s)) => s,
            _ => {
                warn!(%evaluation_id, "input_update expansion: sidecar missing");
                return;
            }
        };
        let task = match ETask::find_by_id(task_id).one(db).await {
            Ok(Some(p)) => p,
            _ => {
                warn!(%task_id, "input_update expansion: task missing");
                return;
            }
        };

        match gradient_ci::trigger::fan_out_expansion(
            db,
            &task,
            sidecar.base_commit,
            matched,
            eval.trigger,
        )
        .await
        {
            Ok(created) => {
                debug!(%evaluation_id, count = created.len(), "fanned out input_update expansion")
            }
            Err(e) => warn!(error = %e, %evaluation_id, "input_update fan-out failed"),
        }
    }

    pub async fn handle_eval_result(
        &self,
        job_id: &str,
        mut derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        let job = match self.active_job(job_id).await {
            Some(PendingJob::Eval(j)) => j,
            Some(_) => anyhow::bail!("job {} is not an eval job", job_id),
            None => {
                warn!(%job_id, "eval result for unknown job - ignoring");
                return Ok(());
            }
        };

        // Canonicalise every store path to its bare `<hash>-<name>` form before
        // it reaches the graph actor: `derivation.derivation_path` mirrors the
        // narinfo `References:` convention used by `cached_path`, and the
        // `/nix/store/` prefix is added back only at the worker / API boundary.
        for d in &mut derivations {
            d.drv_path = strip_nix_store_prefix(&d.drv_path);
            for dep in &mut d.dependencies {
                *dep = strip_nix_store_prefix(dep);
            }
        }

        let Some(evaluation) = EEvaluation::find_by_id(job.evaluation_id)
            .one(&self.state.worker_db)
            .await?
        else {
            anyhow::bail!("evaluation {} not found", job.evaluation_id);
        };
        let facts = eval::assess_substitutability(&self.state, &evaluation, &derivations).await;
        self.state
            .graph
            .ingest(IngestBatch {
                evaluation: job.evaluation_id,
                task: job.task_id,
                derivations,
                warnings,
                errors,
                truly_substituted: facts.truly_substituted,
                upstream_substitutable: facts.upstream_substitutable,
                upstream_hits: facts.upstream_hits,
            })
            .await?;

        Ok(())
    }

    /// Persist a worker-reported message on the evaluation that owns the
    /// given active `job_id`.
    ///
    /// Used for infrastructure-level signals (NAR prefetch failures, transport
    /// errors, etc.) that should surface on the evaluation page even when the
    /// root cause was seen in a sub-job. Build compile failures and
    /// user-initiated aborts deliberately do not flow through here.
    pub async fn record_eval_message(
        &self,
        job_id: &str,
        level: gradient_types::proto::EvalMessageLevel,
        source: String,
        message: String,
    ) -> Result<()> {
        let Some(evaluation_id) = self.active_job(job_id).await.map(|j| j.evaluation_id()) else {
            debug!(%job_id, "EvalMessage dropped: no active job");
            return Ok(());
        };

        let entity_level = match level {
            gradient_types::proto::EvalMessageLevel::Error => {
                gradient_entity::evaluation_message::MessageLevel::Error
            }
            gradient_types::proto::EvalMessageLevel::Warning => {
                gradient_entity::evaluation_message::MessageLevel::Warning
            }
            gradient_types::proto::EvalMessageLevel::Notice => {
                gradient_entity::evaluation_message::MessageLevel::Notice
            }
        };

        gradient_db::insert_evaluation_message(
            &self.state.worker_db,
            evaluation_id,
            entity_level,
            message,
            Some(source),
        )
        .await
        .map_err(Into::into)
    }
}
