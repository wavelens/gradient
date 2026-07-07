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
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            let Some(j) = tracker.active_eval_job(job_id) else {
                return;
            };
            j.evaluation_id
        };
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
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            let Some(j) = tracker.active_eval_job(job_id) else {
                return;
            };
            j.evaluation_id
        };
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

        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            let Some(j) = tracker.active_eval_job(job_id) else {
                return;
            };
            j.evaluation_id
        };

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

    pub async fn handle_eval_result(
        &self,
        job_id: &str,
        mut derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        let job = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => j.clone(),
                Some(_) => anyhow::bail!("job {} is not an eval job", job_id),
                None => {
                    warn!(%job_id, "eval result for unknown job - ignoring");
                    return Ok(());
                }
            }
        };

        // Canonicalise every store path to its bare `<hash>-<name>` form
        // before it reaches the DB. `derivation.derivation_path` mirrors the
        // narinfo `References:` convention used by `cached_path`: the
        // `/nix/store/` prefix is added back only at the worker / API
        // boundary. Worker batches may arrive prefixed (eval), unprefixed
        // (mixed legacy), or both, so we strip uniformly here and keep one
        // canonical form for every downstream key (insert dedup, deferred
        // dep edges, build dispatch lookup).
        for d in &mut derivations {
            d.drv_path = strip_nix_store_prefix(&d.drv_path);
            for dep in &mut d.dependencies {
                *dep = strip_nix_store_prefix(dep);
            }
        }

        // Accumulate this batch's dependency edges. Whatever is fully
        // resolvable after the batch persists is flushed immediately below so
        // builds dispatch mid-stream; the remainder is settled by
        // `flush_deferred_deps` at stream completion.
        {
            let mut acc = self.eval_edges.write().await;
            acc.entry(job.evaluation_id)
                .or_default()
                .add_batch(&derivations);
        }

        eval::handle_eval_result(&self.state, &job, derivations, warnings, errors).await?;

        {
            let mut acc = self.eval_edges.write().await;
            if let Some(entry) = acc.get_mut(&job.evaluation_id)
                && let Err(e) = eval::flush_ready_edges(&self.state, job.evaluation_id, entry).await
            {
                warn!(error = %e, evaluation_id = %job.evaluation_id, "incremental edge flush failed; deferring to completion flush");
            }
        }

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
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(j) => j.evaluation_id(),
                None => {
                    debug!(%job_id, "EvalMessage dropped: no active job");
                    return Ok(());
                }
            }
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
            self.state.worker_db.inner(),
            evaluation_id,
            entity_level,
            message,
            Some(source),
        )
        .await
        .map_err(Into::into)
    }
}
