/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `BuildOutput` messages from workers and build job lifecycle.

use std::collections::HashMap;
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
                if let BuildOutputMetadata::Available { nar_size, nar_hash: _ } = output.nar_metadata()
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
        let build = match EBuild::find_by_id(build_id).one(&self.state.worker_db).await? {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_completed");
                return Ok(());
            }
        };
        let evaluation_id = build.evaluation;
        let leader =
            update_build_status(Arc::clone(self.state), build, BuildStatus::Completed).await;
        self.propagate_to_followers(&leader).await?;
        self.check_evaluation_done(evaluation_id).await
    }

    pub async fn handle_build_job_failed(&self, build_id: BuildId, _error: &str) -> Result<()> {
        let build = match EBuild::find_by_id(build_id).one(&self.state.worker_db).await? {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_failed");
                return Ok(());
            }
        };
        let evaluation_id = build.evaluation;
        let derivation_id = build.derivation;
        let leader = update_build_status(Arc::clone(self.state), build, BuildStatus::Failed).await;
        self.propagate_to_followers(&leader).await?;
        self.cascade_dependency_failed(evaluation_id, derivation_id)
            .await?;
        self.check_evaluation_done(evaluation_id).await
    }

    /// Copy a leader's terminal status (and `log_id`, `build_time_ms`,
    /// `worker`) onto every build with `via = leader.id`, then run the
    /// per-evaluation finalisation each follower needs (`DependencyFailed`
    /// cascade on failure, `check_evaluation_done` to flip the eval).
    ///
    /// Followers always share a `derivation` row with their leader, so
    /// `derivation_output` and `build_product` rows are already visible to
    /// the follower's evaluation without any copy.
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

            let Some(reloaded) =
                EBuild::find_by_id(follower.id).one(&self.state.worker_db).await?
            else {
                continue;
            };
            update_build_status(Arc::clone(self.state), reloaded, leader.status.clone()).await;

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
            update_build_status(Arc::clone(self.state), build, BuildStatus::DependencyFailed)
                .await;
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

    /// Sweep every in-flight evaluation (`Building` or `Waiting`) and reconcile
    /// its status against the current set of connected workers' capabilities.
    ///
    /// - `Building` → `Waiting` when **none** of the eval's still-pending builds
    ///   has a connected worker whose `architectures` + `system_features` can
    ///   satisfy it. Surfaces "no worker configured for these builds" in the UI
    ///   instead of leaving the eval stuck silently.
    /// - `Waiting` → `Building` when **any** pending build now has a matching
    ///   worker (e.g. an aarch64 worker just connected, or an existing worker
    ///   added a new system feature via re-advertised capabilities).
    ///
    /// Cheap: one query for the small set of in-flight evals, one query for
    /// their non-terminal builds + derivations, one query for required features.
    /// Worker caps are taken as a snapshot from the in-memory pool. Safe to call
    /// from the dispatch loop and from worker-capability change hooks.
    pub async fn reconcile_waiting_state(
        &self,
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> Result<()> {
        let evals = EEvaluation::find()
            .filter(
                CEvaluation::Status
                    .is_in(vec![EvaluationStatus::Building, EvaluationStatus::Waiting]),
            )
            .all(&self.state.worker_db)
            .await
            .context("fetch in-flight evaluations")?;
        if evals.is_empty() {
            return Ok(());
        }

        for eval in evals {
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

            if pending_builds.is_empty() {
                // Nothing left to gate — terminal-status decision happens in
                // `check_evaluation_done`, not here.
                continue;
            }

            let drv_ids: Vec<DerivationId> = pending_builds.iter().map(|b| b.derivation).collect();
            let checker = BuildabilityChecker::load(self.state, &drv_ids).await?;
            let target = if checker.any_buildable(&pending_builds, worker_caps) {
                EvaluationStatus::Building
            } else {
                EvaluationStatus::Waiting
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
                update_evaluation_status(Arc::clone(self.state), eval, target).await;
            }
        }

        Ok(())
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
        let drv_by_id: HashMap<DerivationId, MDerivation> = drvs.into_iter().map(|d| (d.id, d)).collect();

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
            let required: Vec<&str> = self
                .features_by_drv
                .get(&b.derivation)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|i| self.feature_name.get(i).map(String::as_str))
                        .collect()
                })
                .unwrap_or_default();
            worker_caps.iter().any(|(arch, feats)| {
                let arch_ok =
                    drv.architecture == "builtin" || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required.iter().all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            })
        })
    }
}
