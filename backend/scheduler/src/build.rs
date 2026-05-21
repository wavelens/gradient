/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `BuildOutput` messages from workers and build job lifecycle.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result};

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::db::{
    collect_transitive_dependents, update_build_status, update_evaluation_status,
};
use gradient_core::types::*;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{error, info, warn};

use super::jobs::PendingBuildJob;
use gradient_core::types::BuildOutputMetadata;
use gradient_core::types::proto::BuildOutput;

/// Wraps `&ServerState` so build-lifecycle helpers don't repeat `state` as a parameter.
pub(crate) struct BuildStateHandler<'a> {
    state: &'a Arc<ServerState>,
}

impl<'a> BuildStateHandler<'a> {
    pub(crate) fn new(state: &'a Arc<ServerState>) -> Self {
        Self { state }
    }

    pub async fn handle_build_output(
        &self,
        _job: &PendingBuildJob,
        build_id: BuildId,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        let build = EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await
            .context("fetch build")?
            .with_context(|| format!("build {} not found", build_id))?;

        let derivation_id = build.derivation;

        for output in &outputs {
            let existing = EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.eq(derivation_id))
                .filter(CDerivationOutput::Name.eq(&output.name))
                .one(&self.state.worker_db)
                .await
                .context("fetch derivation_output")?;

            if let Some(row) = existing {
                let row_id = row.id;
                let mut active = row.into_active_model();
                if let BuildOutputMetadata::Available {
                    nar_size,
                    nar_hash: _,
                } = output.nar_metadata()
                {
                    active.nar_size = Set(Some(nar_size));
                }
                if let Err(e) = active.update(&self.state.worker_db).await {
                    error!(error = %e, %build_id, output_name = %output.name, "failed to update derivation_output");
                }

                // Delete any prior products for this output (idempotency on retry).
                if let Err(e) = EBuildProduct::delete_many()
                    .filter(CBuildProduct::DerivationOutput.eq(row_id))
                    .exec(&self.state.worker_db)
                    .await
                    .context("delete prior build_product rows")
                {
                    warn!(error = %e, %build_id, output_name = %output.name, "failed to delete prior build_product rows");
                }

                // Insert new product rows.
                for product in &output.products {
                    let am = ABuildProduct {
                        id: Set(BuildProductId::now_v7()),
                        derivation_output: Set(row_id),
                        file_type: Set(product.file_type.clone()),
                        subtype: Set(product.subtype.clone()),
                        name: Set(product.name.clone()),
                        path: Set(product.path.clone()),
                        size: Set(product.size.map(|s| s as i64)),
                        created_at: Set(gradient_core::types::now()),
                    };
                    if let Err(e) = am.insert(&self.state.worker_db).await {
                        warn!(error = %e, %build_id, output_name = %output.name, "failed to insert build_product");
                    }
                }
            } else {
                warn!(%build_id, output_name = %output.name, "derivation_output row not found");
            }
        }

        info!(%build_id, output_count = outputs.len(), "build outputs recorded");
        Ok(())
    }

    pub async fn handle_build_job_completed(&self, build_id: BuildId) -> Result<()> {
        let build = match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await?
        {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_completed");
                return Ok(());
            }
        };
        let evaluation_id = build.evaluation;
        let derivation_id = build.derivation;
        let was_external_cached = build.external_cached;
        let leader =
            update_build_status(Arc::clone(self.state), build, BuildStatus::Completed).await;
        self.propagate_to_followers(&leader).await?;

        if was_external_cached {
            let state = Arc::clone(self.state);
            let leader_id = leader.id;
            tokio::spawn(async move {
                let drv_path = match EDerivation::find_by_id(derivation_id)
                    .one(&state.worker_db)
                    .await
                {
                    Ok(Some(d)) => d.drv_path(),
                    Ok(None) => {
                        warn!(%leader_id, %derivation_id, "substitute_log: derivation row missing");
                        return;
                    }
                    Err(e) => {
                        warn!(%leader_id, error = %e, "substitute_log: derivation lookup failed");
                        return;
                    }
                };
                if let Err(e) = crate::log_substitution::substitute_log(
                    state, leader_id, derivation_id, drv_path, true,
                )
                .await
                {
                    warn!(%leader_id, error = %e, "substitute_log spawn failed");
                }
            });
        }

        self.check_evaluation_done(evaluation_id).await
    }

    pub async fn handle_build_job_failed(&self, build_id: BuildId, error: &str) -> Result<()> {
        let build = match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await?
        {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_failed");
                return Ok(());
            }
        };

        // Surface the worker's failure reason in the build log so the
        // frontend's log viewer renders it. Without this, pre-`nix build`
        // aborts (prefetch-time errors, daemon connection failures, etc.)
        // produce a Failed badge with an empty log — useless for diagnosis.
        // Followers share this `log_id` via `propagate_to_followers`, so a
        // single append covers the whole leader→followers fan-out.
        let log_id = build.log_id.unwrap_or(build.id);
        if let Err(e) = self
            .state
            .log_storage
            .append(log_id, &format!("\n=== build failed: {error} ===\n"))
            .await
        {
            warn!(%build_id, error = %e, "failed to append worker error to build log");
        }

        let evaluation_id = build.evaluation;
        let derivation_id = build.derivation;
        let leader = update_build_status(Arc::clone(self.state), build, BuildStatus::Failed).await;
        self.propagate_to_followers(&leader).await?;
        self.cascade_dependency_failed(evaluation_id, derivation_id)
            .await?;
        self.check_evaluation_done(evaluation_id).await
    }

    /// Copy a leader's terminal status (and `log_id`, `build_time_ms`, `worker`)
    /// onto every build with `via = leader.id`, then run the per-evaluation
    /// finalisation each follower needs (`DependencyFailed` cascade on failure,
    /// `check_evaluation_done` to flip the eval).
    ///
    /// Same-org followers share the leader's `derivation` row, so its
    /// `derivation_output` and `build_product` children are already visible to
    /// the follower's evaluation without any copy. Cross-org followers (whose
    /// `derivation` differs from the leader's — created when the leader belongs
    /// to a cache-connected organisation) have those rows mirrored onto the
    /// follower's `derivation`.
    ///
    /// `Aborted` is not propagated — when a leader is aborted (its eval was
    /// cancelled), callers re-elect a new leader from the followers instead.
    async fn propagate_to_followers(&self, leader: &MBuild) -> Result<()> {
        let propagate = matches!(
            leader.status,
            BuildStatus::Completed
                | BuildStatus::Substituted
                | BuildStatus::Failed
                | BuildStatus::DependencyFailed
        );
        if !propagate {
            return Ok(());
        }

        let followers = EBuild::find()
            .filter(CBuild::Via.eq(leader.id))
            .all(&self.state.worker_db)
            .await
            .context("fetch followers")?;
        if followers.is_empty() {
            return Ok(());
        }

        let leader_outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(leader.derivation))
            .all(&self.state.worker_db)
            .await
            .context("fetch leader's derivation_output rows")?;
        let leader_output_ids: Vec<_> = leader_outputs.iter().map(|o| o.id).collect();
        let leader_products = if leader_output_ids.is_empty() {
            Vec::new()
        } else {
            EBuildProduct::find()
                .filter(CBuildProduct::DerivationOutput.is_in(leader_output_ids))
                .all(&self.state.worker_db)
                .await
                .context("fetch leader's build_product rows")?
        };

        for follower in followers {
            let evaluation_id = follower.evaluation;
            let derivation_id = follower.derivation;
            let mut active: ABuild = follower.clone().into_active_model();
            active.log_id = Set(leader.log_id);
            active.build_time_ms = Set(leader.build_time_ms);
            active.worker = Set(leader.worker.clone());
            active.via = Set(None);
            if let Err(e) = active.update(&self.state.worker_db).await {
                error!(error = %e, follower_id = %follower.id, "failed to copy leader fields to follower");
                continue;
            }

            let Some(reloaded) = EBuild::find_by_id(follower.id)
                .one(&self.state.worker_db)
                .await?
            else {
                continue;
            };
            update_build_status(Arc::clone(self.state), reloaded, leader.status).await;

            if follower.derivation != leader.derivation {
                let existing_outs = EDerivationOutput::find()
                    .filter(CDerivationOutput::Derivation.eq(follower.derivation))
                    .all(&self.state.worker_db)
                    .await
                    .context("fetch follower's existing derivation_output rows")?;
                let existing_out_ids: Vec<_> = existing_outs.iter().map(|o| o.id).collect();
                if !existing_out_ids.is_empty() {
                    if let Err(e) = EBuildProduct::delete_many()
                        .filter(CBuildProduct::DerivationOutput.is_in(existing_out_ids.clone()))
                        .exec(&self.state.worker_db)
                        .await
                    {
                        warn!(error = %e, follower_id = %follower.id, "failed to clear stale follower build_products");
                    }
                    if let Err(e) = EDerivationOutput::delete_many()
                        .filter(CDerivationOutput::Id.is_in(existing_out_ids))
                        .exec(&self.state.worker_db)
                        .await
                    {
                        warn!(error = %e, follower_id = %follower.id, "failed to clear stale follower derivation_outputs");
                    }
                }

                let (new_outputs, new_products) =
                    build_cross_org_artefact_rows(follower.derivation, &leader_outputs, &leader_products);

                for out in new_outputs {
                    if let Err(e) = out.insert(&self.state.worker_db).await {
                        warn!(error = %e, follower_id = %follower.id, "failed to mirror derivation_output to follower");
                    }
                }
                for product in new_products {
                    if let Err(e) = product.insert(&self.state.worker_db).await {
                        warn!(error = %e, follower_id = %follower.id, "failed to mirror build_product to follower");
                    }
                }
            }

            if matches!(
                leader.status,
                BuildStatus::Failed | BuildStatus::DependencyFailed
            ) {
                self.cascade_dependency_failed(evaluation_id, derivation_id)
                    .await?;
            }
            self.check_evaluation_done(evaluation_id).await?;
        }

        Ok(())
    }

    async fn cascade_dependency_failed(
        &self,
        evaluation_id: EvaluationId,
        failed_derivation_id: DerivationId,
    ) -> Result<()> {
        let mut closure =
            collect_transitive_dependents(&self.state.worker_db, failed_derivation_id).await?;
        // The failed derivation itself was already marked Failed by the caller;
        // only its dependents need DependencyFailed.
        closure.remove(&failed_derivation_id);
        if closure.is_empty() {
            return Ok(());
        }

        let cascaded_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.is_in(vec![BuildStatus::Created, BuildStatus::Queued]))
            .filter(CBuild::Derivation.is_in(closure.into_iter().collect::<Vec<_>>()))
            .all(&self.state.worker_db)
            .await
            .context("fetch builds for cascade")?;

        for build in cascaded_builds {
            update_build_status(Arc::clone(self.state), build, BuildStatus::DependencyFailed).await;
        }
        Ok(())
    }

    /// Transitions the evaluation to its final state if all builds are done.
    ///
    /// Returns early if any build is still active (Created/Queued/Building) or if
    /// the evaluation is not in `Building` state. Otherwise sets `Failed` when at
    /// least one build failed (Failed or DependencyFailed), else `Completed`.
    pub(crate) async fn check_evaluation_done(&self, evaluation_id: EvaluationId) -> Result<()> {
        let active = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .all(&self.state.worker_db)
            .await
            .context("fetch active builds")?;

        if !active.is_empty() {
            return Ok(());
        }

        let Some(eval) = EEvaluation::find_by_id(evaluation_id)
            .one(&self.state.worker_db)
            .await?
        else {
            return Ok(());
        };

        if !matches!(eval.status, EvaluationStatus::Building) {
            return Ok(());
        }

        let failed_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.is_in(vec![BuildStatus::Failed, BuildStatus::DependencyFailed]))
            .all(&self.state.worker_db)
            .await
            .context("fetch failed builds")?;

        // Also treat error-level evaluation messages (nix eval errors, attr
        // resolution failures) as a failure signal — the evaluation was only
        // partially successful even if every discovered build passed.
        let eval_error_messages = EEvaluationMessage::find()
            .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
            .filter(CEvaluationMessage::Level.eq(entity::evaluation_message::MessageLevel::Error))
            .all(&self.state.worker_db)
            .await
            .context("fetch eval error messages")?;

        let target = if failed_builds.is_empty() && eval_error_messages.is_empty() {
            EvaluationStatus::Completed
        } else {
            EvaluationStatus::Failed
        };
        info!(
            %evaluation_id,
            ?target,
            failed_builds = failed_builds.len(),
            eval_errors = eval_error_messages.len(),
            "evaluation finished"
        );
        update_evaluation_status(Arc::clone(self.state), eval, target).await;
        Ok(())
    }

    /// Sweep every in-flight evaluation and reconcile its status against the
    /// current set of connected workers.
    ///
    /// Two regimes apply, keyed on whether the eval has any pending builds:
    ///
    /// - **Pre-build phase** (no pending builds yet, status in
    ///   `Queued`/`Fetching`/`EvaluatingFlake`/`EvaluatingDerivation`): if no
    ///   worker is connected, flip to `Waiting` so the UI can explain why the
    ///   eval is stuck. A `Waiting` eval with no pending builds came from this
    ///   path; if a worker has since connected, recover to `Queued` and let the
    ///   dispatch loop replay the normal progression.
    /// - **Build phase** (pending builds present, status in
    ///   `Building`/`Waiting`): flip `Building ↔ Waiting` based on whether any
    ///   pending build's `(architecture, required_features)` is satisfiable by
    ///   the connected pool, persisting the structured reason for the UI.
    pub async fn reconcile_waiting_state(
        &self,
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> Result<()> {
        let evals = EEvaluation::find()
            .filter(CEvaluation::Status.is_in(vec![
                EvaluationStatus::Queued,
                EvaluationStatus::Fetching,
                EvaluationStatus::EvaluatingFlake,
                EvaluationStatus::EvaluatingDerivation,
                EvaluationStatus::Building,
                EvaluationStatus::Waiting,
            ]))
            .all(&self.state.worker_db)
            .await
            .context("fetch in-flight evaluations")?;
        if evals.is_empty() {
            return Ok(());
        }

        for eval in evals {
            // Approval and no-cache parks are owned by webhook + cache-create
            // hooks. The reconciler must not unpark them just because workers
            // showed up.
            if eval.status == EvaluationStatus::Waiting
                && eval
                    .waiting_reason
                    .as_ref()
                    .and_then(WaitingReason::from_json)
                    .is_some_and(|r| !matches!(r, WaitingReason::Workers { .. }))
            {
                continue;
            }

            let pending_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(eval.id))
                .filter(CBuild::Status.is_in(vec![
                    BuildStatus::Created,
                    BuildStatus::Queued,
                    BuildStatus::Building,
                ]))
                .all(&self.state.worker_db)
                .await
                .context("fetch pending builds")?;

            let (target, new_reason) = if pending_builds.is_empty() {
                match eval.status {
                    EvaluationStatus::Building => continue,
                    EvaluationStatus::Queued
                    | EvaluationStatus::Fetching
                    | EvaluationStatus::EvaluatingFlake
                    | EvaluationStatus::EvaluatingDerivation
                    | EvaluationStatus::Waiting => {
                        match decide_pre_build_target(eval.status, worker_caps.len()) {
                            Some(pair) => pair,
                            None => continue,
                        }
                    }
                    _ => continue,
                }
            } else {
                if !matches!(
                    eval.status,
                    EvaluationStatus::Building | EvaluationStatus::Waiting
                ) {
                    continue;
                }
                let drv_ids: Vec<DerivationId> =
                    pending_builds.iter().map(|b| b.derivation).collect();
                let checker = BuildabilityChecker::load(self.state, &drv_ids).await?;
                let target = if checker.any_buildable(&pending_builds, worker_caps) {
                    EvaluationStatus::Building
                } else {
                    EvaluationStatus::Waiting
                };
                let reason = if matches!(target, EvaluationStatus::Waiting) {
                    Some(checker.compute_waiting_reason(&pending_builds, worker_caps))
                } else {
                    None
                };
                (target, reason)
            };

            if eval.status != target {
                info!(
                    evaluation_id = %eval.id,
                    from = ?eval.status,
                    to = ?target,
                    pending = pending_builds.len(),
                    workers = worker_caps.len(),
                    "reconciling evaluation waiting state"
                );
            }

            persist_waiting_reason(
                self.state,
                eval.id,
                &eval.waiting_reason,
                new_reason.as_ref(),
            )
            .await;

            if eval.status != target {
                update_evaluation_status(Arc::clone(self.state), eval, target).await;
            }
        }

        Ok(())
    }
}

/// Decides whether a pre-build evaluation needs to be reconciled.
///
/// Returns `Some((target, reason))` when the evaluation should transition or
/// have its waiting reason refreshed; `None` when it is actively progressing
/// and must be left alone.
///
/// Active pre-build states (`Fetching`, `EvaluatingFlake`,
/// `EvaluatingDerivation`) are owned by an eval worker. They may only stall
/// into `Waiting` if every worker has disconnected — they must never be
/// reset back to `Queued`, which the `EvalStateMachine` forbids and which
/// would clobber the worker's in-flight progress.
fn decide_pre_build_target(
    current: EvaluationStatus,
    connected_workers: usize,
) -> Option<(EvaluationStatus, Option<WaitingReason>)> {
    let stall = || {
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::Workers {
                unmet: Vec::new(),
                connected_workers: 0,
                available_architectures: Vec::new(),
            }),
        )
    };
    match (current, connected_workers) {
        (EvaluationStatus::Waiting, 0) => Some(stall()),
        (EvaluationStatus::Waiting, _) => Some((EvaluationStatus::Queued, None)),
        (
            EvaluationStatus::Queued
            | EvaluationStatus::Fetching
            | EvaluationStatus::EvaluatingFlake
            | EvaluationStatus::EvaluatingDerivation,
            0,
        ) => Some(stall()),
        _ => None,
    }
}

/// Update `evaluation.waiting_reason` only when the value actually changes.
///
/// Avoids a row-level UPDATE every reconcile cycle when the unmet capabilities
/// haven't shifted, which keeps `updated_at` from churning on the row.
async fn persist_waiting_reason(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    current: &Option<serde_json::Value>,
    new_reason: Option<&WaitingReason>,
) {
    let new_value = new_reason.map(|r| r.to_json());

    let unchanged = match (current, &new_value) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if unchanged {
        return;
    }

    let res = EEvaluation::update_many()
        .col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(new_value),
        )
        .filter(CEvaluation::Id.eq(evaluation_id))
        .exec(&state.worker_db)
        .await;

    if let Err(e) = res {
        warn!(error = %e, %evaluation_id, "failed to persist waiting_reason");
    }
}

// ---------------------------------------------------------------------------
// Public free-function API (thin wrappers around BuildStateHandler)
// ---------------------------------------------------------------------------

pub async fn handle_build_output(
    state: &Arc<ServerState>,
    job: &PendingBuildJob,
    build_id: BuildId,
    outputs: Vec<BuildOutput>,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_output(job, build_id, outputs)
        .await
}

pub async fn handle_build_job_completed(state: &Arc<ServerState>, build_id: BuildId) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_job_completed(build_id)
        .await
}

pub async fn handle_build_job_failed(
    state: &Arc<ServerState>,
    build_id: BuildId,
    error: &str,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_job_failed(build_id, error)
        .await
}

pub(crate) async fn check_evaluation_done(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<()> {
    BuildStateHandler::new(state)
        .check_evaluation_done(evaluation_id)
        .await
}

pub async fn reconcile_waiting_state(
    state: &Arc<ServerState>,
    worker_caps: &[(Vec<String>, Vec<String>)],
) -> Result<()> {
    BuildStateHandler::new(state)
        .reconcile_waiting_state(worker_caps)
        .await
}

/// Build the `derivation_output` and `build_product` rows to insert under a
/// cross-org follower's `derivation` so its evaluation can resolve artefacts
/// without org-aware indirection. Pure: no DB, no I/O.
///
/// Returns `(new_outputs, new_products)`. `new_outputs[i]` has a fresh
/// `DerivationOutputId` and `derivation = follower_derivation`; every other
/// column is copied from the corresponding leader row. `new_products` rewrites
/// the `derivation_output` FK to point at the matching new output id.
pub(crate) fn build_cross_org_artefact_rows(
    follower_derivation: DerivationId,
    leader_outputs: &[MDerivationOutput],
    leader_products: &[MBuildProduct],
) -> (Vec<ADerivationOutput>, Vec<ABuildProduct>) {
    use std::collections::HashMap;
    use entity::ids::{BuildProductId, DerivationOutputId};

    let mut old_to_new: HashMap<DerivationOutputId, DerivationOutputId> = HashMap::new();
    let new_outputs: Vec<ADerivationOutput> = leader_outputs
        .iter()
        .map(|src| {
            let new_id = DerivationOutputId::now_v7();
            old_to_new.insert(src.id, new_id);
            ADerivationOutput {
                id: Set(new_id),
                derivation: Set(follower_derivation),
                name: Set(src.name.clone()),
                hash: Set(src.hash.clone()),
                package: Set(src.package.clone()),
                ca: Set(src.ca.clone()),
                nar_size: Set(src.nar_size),
                is_cached: Set(src.is_cached),
                cached_path: Set(src.cached_path),
                created_at: Set(src.created_at),
            }
        })
        .collect();

    let new_products: Vec<ABuildProduct> = leader_products
        .iter()
        .filter_map(|src| {
            let new_output_id = old_to_new.get(&src.derivation_output).copied()?;
            Some(ABuildProduct {
                id: Set(BuildProductId::now_v7()),
                derivation_output: Set(new_output_id),
                file_type: Set(src.file_type.clone()),
                subtype: Set(src.subtype.clone()),
                name: Set(src.name.clone()),
                path: Set(src.path.clone()),
                size: Set(src.size),
                created_at: Set(src.created_at),
            })
        })
        .collect();

    (new_outputs, new_products)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Pre-loaded derivation and feature data for a set of pending builds.
///
/// Used by [`BuildStateHandler::reconcile_waiting_state`] to determine whether
/// any pending build can be satisfied by the current worker pool without
/// re-querying the DB per evaluation.
struct BuildabilityChecker {
    drv_by_id: HashMap<DerivationId, MDerivation>,
    /// Maps derivation ID → list of required feature IDs.
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_name: HashMap<FeatureId, String>,
}

impl BuildabilityChecker {
    /// Query the DB for all derivations and required features referenced by
    /// `drv_ids`, returning a checker ready to call [`any_buildable`].
    ///
    /// [`any_buildable`]: BuildabilityChecker::any_buildable
    async fn load(state: &Arc<ServerState>, drv_ids: &[DerivationId]) -> Result<Self> {
        let drvs = EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids.to_vec()))
            .all(&state.worker_db)
            .await
            .context("fetch derivations for pending builds")?;
        let drv_by_id: HashMap<DerivationId, MDerivation> =
            drvs.into_iter().map(|d| (d.id, d)).collect();

        let edges = EDerivationFeature::find()
            .filter(CDerivationFeature::Derivation.is_in(drv_ids.to_vec()))
            .all(&state.worker_db)
            .await
            .context("fetch derivation_feature edges")?;
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        for e in &edges {
            features_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.feature);
        }

        let feature_ids: Vec<FeatureId> = edges.iter().map(|e| e.feature).collect();
        let feature_rows = if feature_ids.is_empty() {
            vec![]
        } else {
            EFeature::find()
                .filter(CFeature::Id.is_in(feature_ids))
                .all(&state.worker_db)
                .await
                .context("fetch feature names")?
        };
        let feature_name: HashMap<FeatureId, String> =
            feature_rows.into_iter().map(|f| (f.id, f.name)).collect();

        Ok(Self {
            drv_by_id,
            features_by_drv,
            feature_name,
        })
    }

    /// Returns `true` if at least one build in `builds` can be satisfied by
    /// some worker in `worker_caps`:
    /// `(build.arch ∈ worker.architectures) ∧ (∀ required feature ∈ worker.system_features)`.
    fn any_buildable(&self, builds: &[MBuild], worker_caps: &[(Vec<String>, Vec<String>)]) -> bool {
        builds.iter().any(|b| {
            let Some(drv) = self.drv_by_id.get(&b.derivation) else {
                return false;
            };
            let required: Vec<&str> = self.required_features_for(&b.derivation);
            worker_caps.iter().any(|(arch, feats)| {
                let arch_ok =
                    drv.architecture == "builtin" || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required.iter().all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            })
        })
    }

    fn required_features_for(&self, drv_id: &DerivationId) -> Vec<&str> {
        self.features_by_drv
            .get(drv_id)
            .map(|ids| {
                let mut names: Vec<&str> = ids
                    .iter()
                    .filter_map(|i| self.feature_name.get(i).map(String::as_str))
                    .collect();
                names.sort_unstable();
                names.dedup();
                names
            })
            .unwrap_or_default()
    }

    /// Group every unsatisfiable `(architecture, required_features)` combo and
    /// the number of pending builds it covers. Used for the API
    /// `waiting_reason` payload so the UI can explain *why* nothing is
    /// dispatching.
    fn compute_waiting_reason(
        &self,
        builds: &[MBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> WaitingReason {
        let mut grouped: BTreeMap<(String, Vec<String>), u32> = BTreeMap::new();
        for b in builds {
            let Some(drv) = self.drv_by_id.get(&b.derivation) else {
                continue;
            };
            let required_owned: Vec<String> = self
                .required_features_for(&b.derivation)
                .into_iter()
                .map(str::to_owned)
                .collect();
            let satisfied = worker_caps.iter().any(|(arch, feats)| {
                let arch_ok =
                    drv.architecture == "builtin" || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required_owned
                    .iter()
                    .all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            });
            if satisfied {
                continue;
            }
            *grouped
                .entry((drv.architecture.clone(), required_owned))
                .or_default() += 1;
        }

        let unmet: Vec<UnmetRequirement> = grouped
            .into_iter()
            .map(
                |((architecture, required_features), build_count)| UnmetRequirement {
                    architecture,
                    required_features,
                    build_count,
                },
            )
            .collect();

        let mut available_architectures: Vec<String> = worker_caps
            .iter()
            .flat_map(|(archs, _)| archs.iter().cloned())
            .collect();
        available_architectures.sort_unstable();
        available_architectures.dedup();

        WaitingReason::Workers {
            unmet,
            connected_workers: worker_caps.len() as u32,
            available_architectures,
        }
    }
}

#[cfg(test)]
mod waiting_reason_tests {
    use super::*;

    fn workers_view(r: &WaitingReason) -> (&[UnmetRequirement], u32, &[String]) {
        match r {
            WaitingReason::Workers {
                unmet,
                connected_workers,
                available_architectures,
            } => (unmet, *connected_workers, available_architectures),
            other => panic!("expected Workers variant, got {other:?}"),
        }
    }

    fn drv(id: DerivationId, arch: &str) -> MDerivation {
        entity::derivation::Model {
            id,
            organization: OrganizationId::nil(),
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "x".into(),
            architecture: arch.into(),
            created_at: chrono::NaiveDateTime::default(),
        }
    }

    fn build_for(drv_id: DerivationId, eval_id: EvaluationId) -> MBuild {
        entity::build::Model {
            id: BuildId::now_v7(),
            evaluation: eval_id,
            derivation: drv_id,
            status: BuildStatus::Queued,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached: false,
            created_at: chrono::NaiveDateTime::default(),
            updated_at: chrono::NaiveDateTime::default(),
        }
    }

    fn checker_with(
        drvs: Vec<MDerivation>,
        feature_edges: Vec<(DerivationId, FeatureId, &'static str)>,
    ) -> BuildabilityChecker {
        let drv_by_id = drvs.into_iter().map(|d| (d.id, d)).collect();
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        let mut feature_name: HashMap<FeatureId, String> = HashMap::new();
        for (drv_id, feat_id, name) in feature_edges {
            features_by_drv.entry(drv_id).or_default().push(feat_id);
            feature_name.insert(feat_id, name.to_string());
        }
        BuildabilityChecker {
            drv_by_id,
            features_by_drv,
            feature_name,
        }
    }

    #[test]
    fn no_workers_lists_every_unique_arch() {
        let eval_id = EvaluationId::now_v7();
        let d1 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d2 = drv(DerivationId::now_v7(), "x86_64-linux");
        let builds = vec![build_for(d1.id, eval_id), build_for(d2.id, eval_id)];
        let checker = checker_with(vec![d1, d2], vec![]);

        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);

        assert_eq!(connected_workers, 0);
        assert!(available_architectures.is_empty());
        assert_eq!(unmet.len(), 2);
        assert!(
            unmet
                .iter()
                .any(|u| u.architecture == "aarch64-linux" && u.build_count == 1)
        );
        assert!(
            unmet
                .iter()
                .any(|u| u.architecture == "x86_64-linux" && u.build_count == 1)
        );
    }

    #[test]
    fn satisfied_builds_are_excluded_from_unmet() {
        let eval_id = EvaluationId::now_v7();
        let d_x86 = drv(DerivationId::now_v7(), "x86_64-linux");
        let d_arm = drv(DerivationId::now_v7(), "aarch64-linux");
        let builds = vec![build_for(d_x86.id, eval_id), build_for(d_arm.id, eval_id)];
        let checker = checker_with(vec![d_x86, d_arm], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);

        assert_eq!(connected_workers, 1);
        assert_eq!(available_architectures, ["x86_64-linux"]);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
        assert_eq!(unmet[0].build_count, 1);
    }

    #[test]
    fn missing_feature_is_reported_alongside_arch() {
        let eval_id = EvaluationId::now_v7();
        let drv_id = DerivationId::now_v7();
        let feat_id = FeatureId::now_v7();
        let d = drv(drv_id, "x86_64-linux");
        let builds = vec![build_for(drv_id, eval_id)];
        let checker = checker_with(vec![d], vec![(drv_id, feat_id, "kvm")]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);

        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "x86_64-linux");
        assert_eq!(unmet[0].required_features, vec!["kvm".to_string()]);
        assert_eq!(unmet[0].build_count, 1);
    }

    #[test]
    fn identical_requirements_are_grouped_with_count() {
        let eval_id = EvaluationId::now_v7();
        let d1 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d2 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d3 = drv(DerivationId::now_v7(), "aarch64-linux");
        let builds = vec![
            build_for(d1.id, eval_id),
            build_for(d2.id, eval_id),
            build_for(d3.id, eval_id),
        ];
        let checker = checker_with(vec![d1, d2, d3], vec![]);

        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, _, _) = workers_view(&reason);

        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
        assert_eq!(unmet[0].build_count, 3);
    }

    #[test]
    fn pre_build_target_queued_no_workers_stalls_to_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Queued, 0)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let reason = reason.expect("stall target must carry a reason");
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);
        assert_eq!(connected_workers, 0);
        assert!(unmet.is_empty());
        assert!(available_architectures.is_empty());
    }

    #[test]
    fn pre_build_target_waiting_with_workers_recovers_to_queued() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Waiting, 2)
            .expect("recovery must produce a transition");
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());
    }

    #[test]
    fn pre_build_target_waiting_no_workers_keeps_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Waiting, 0)
            .expect("waiting must refresh its reason");
        assert_eq!(target, EvaluationStatus::Waiting);
        assert!(reason.is_some());
    }

    #[test]
    fn pre_build_target_queued_with_workers_is_noop() {
        assert!(decide_pre_build_target(EvaluationStatus::Queued, 1).is_none());
    }

    #[test]
    fn pre_build_target_active_pre_build_with_workers_left_alone() {
        // Regression: a Fetching/EvaluatingFlake/EvaluatingDerivation eval is
        // already being processed by an eval worker. Reconcile must not push
        // it back to Queued — that violates the state machine and would log a
        // spurious "invalid status transition: Fetching → Queued" warning.
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            assert!(
                decide_pre_build_target(status, 1).is_none(),
                "{status:?} with workers connected must not be reconciled"
            );
        }
    }

    #[test]
    fn pre_build_target_active_pre_build_no_workers_stalls() {
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            let (target, reason) = decide_pre_build_target(status, 0)
                .unwrap_or_else(|| panic!("{status:?} with no workers must stall"));
            assert_eq!(target, EvaluationStatus::Waiting);
            assert!(reason.is_some());
        }
    }

    #[test]
    fn builtin_arch_satisfied_by_any_worker() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "builtin");
        let builds = vec![build_for(d.id, eval_id)];
        let checker = checker_with(vec![d], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);

        assert!(unmet.is_empty());
    }
}

#[cfg(test)]
mod cross_org_mirror_tests {
    use super::*;
    use entity::ids::{BuildProductId, DerivationId, DerivationOutputId};

    fn output_fixture(id: DerivationOutputId, derivation: DerivationId, hash: &str) -> MDerivationOutput {
        MDerivationOutput {
            id,
            derivation,
            name: "out".into(),
            hash: hash.into(),
            package: "foo".into(),
            ca: None,
            nar_size: Some(1024),
            is_cached: true,
            cached_path: None,
            created_at: chrono::NaiveDateTime::default(),
        }
    }

    fn product_fixture(id: BuildProductId, output: DerivationOutputId) -> MBuildProduct {
        MBuildProduct {
            id,
            derivation_output: output,
            file_type: "file".into(),
            subtype: "doc".into(),
            name: "readme".into(),
            path: "share/doc/readme".into(),
            size: Some(512),
            created_at: chrono::NaiveDateTime::default(),
        }
    }

    #[test]
    fn mirrors_outputs_and_rewrites_product_foreign_keys() {
        let leader_drv = DerivationId::now_v7();
        let follower_drv = DerivationId::now_v7();
        let leader_out_id = DerivationOutputId::now_v7();
        let leader_outputs = vec![output_fixture(leader_out_id, leader_drv, "deadbeef")];
        let leader_products = vec![product_fixture(BuildProductId::now_v7(), leader_out_id)];

        let (new_outputs, new_products) =
            build_cross_org_artefact_rows(follower_drv, &leader_outputs, &leader_products);

        assert_eq!(new_outputs.len(), 1);
        let mirrored_out = &new_outputs[0];
        match &mirrored_out.derivation {
            Set(d) => assert_eq!(*d, follower_drv),
            _ => panic!("derivation not set"),
        }
        match &mirrored_out.hash {
            Set(h) => assert_eq!(h, "deadbeef"),
            _ => panic!("hash not set"),
        }
        match &mirrored_out.id {
            Set(new_id) => assert_ne!(*new_id, leader_out_id),
            _ => panic!("id not set"),
        }

        assert_eq!(new_products.len(), 1);
        let mirrored_product = &new_products[0];
        match (&mirrored_out.id, &mirrored_product.derivation_output) {
            (Set(out_id), Set(prod_out)) => assert_eq!(prod_out, out_id),
            _ => panic!("product FK not rewritten"),
        }
    }

    #[test]
    fn dangling_product_without_owning_output_is_dropped() {
        let follower_drv = DerivationId::now_v7();
        let leader_outputs: Vec<MDerivationOutput> = vec![];
        let leader_products = vec![product_fixture(
            BuildProductId::now_v7(),
            DerivationOutputId::now_v7(),
        )];

        let (new_outputs, new_products) =
            build_cross_org_artefact_rows(follower_drv, &leader_outputs, &leader_products);

        assert!(new_outputs.is_empty());
        assert!(new_products.is_empty(), "orphan product must not be inserted");
    }
}
