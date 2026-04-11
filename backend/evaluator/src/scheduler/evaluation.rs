/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::evaluation_message::MessageLevel;
use gradient_core::ci::CiStatus;
use gradient_core::db::{
    record_evaluation_message, update_build_status, update_evaluation_status,
    update_evaluation_status_with_error,
};
use gradient_core::types::*;
use sea_orm::{
    ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use sea_orm::ActiveValue;
use std::sync::Arc;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use crate::eval::evaluate;
use super::ci::{report_ci_for_entry_points, report_ci_for_evaluation};

#[instrument(skip(state), fields(evaluation_id = %evaluation.id))]
pub async fn schedule_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    info!("Reviewing evaluation");

    // Report the top-level "gradient" check as Running so GitHub/Gitea shows
    // the evaluation is in progress before nix eval even starts.
    if let Some(project_id) = evaluation.project {
        report_ci_for_evaluation(
            Arc::clone(&state),
            project_id,
            evaluation.commit,
            &evaluation.repository,
            evaluation.id,
            CiStatus::Running,
        )
        .await;
    }

    let builds = evaluate(Arc::clone(&state), &evaluation).await;

    match builds {
        Ok(result) => {
            // Re-fetch to check if aborted while evaluate() was running
            match EEvaluation::find_by_id(evaluation.id).one(&state.db).await {
                Ok(Some(current)) if current.status == EvaluationStatus::Aborted => return,
                _ => {}
            }

            info!(
                build_count = result.builds.len(),
                new_derivation_count = result.derivations.len(),
                dependency_count = result.derivation_dependencies.len(),
                "Created builds + derivations"
            );

            const BATCH_SIZE: usize = 1000;

            // 1. Derivations first (builds + derivation_output FK into it).
            if !result.derivations.is_empty() {
                let active: Vec<ADerivation> = result.derivations
                    .iter()
                    .map(|d| d.clone().into_active_model())
                    .collect();
                for chunk in active.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivation::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivations");
                        update_evaluation_status_with_error(
                            Arc::clone(&state),
                            evaluation,
                            EvaluationStatus::Failed,
                            format!("Failed to insert derivations: {}", e),
                            Some("db-insert".to_string()),
                        )
                        .await;
                        return;
                    }
                }
            }

            // 2. Derivation outputs.
            if !result.derivation_outputs.is_empty() {
                for chunk in result.derivation_outputs.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivation outputs");
                    }
                }
            }

            // 3. Derivation dependency edges.
            if !result.derivation_dependencies.is_empty() {
                let active: Vec<ADerivationDependency> = result.derivation_dependencies
                    .iter()
                    .map(|d| d.clone().into_active_model())
                    .collect();
                for chunk in active.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivationDependency::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivation dependencies");
                    }
                }
            }

            // 4. Builds (FK into derivation).
            let active_builds = result.builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            if !active_builds.is_empty() {
                for chunk in active_builds.chunks(BATCH_SIZE) {
                    if let Err(e) = EBuild::insert_many(chunk.to_vec()).exec(&state.db).await {
                        error!(error = %e, "Failed to insert builds");
                        update_evaluation_status_with_error(
                            Arc::clone(&state),
                            evaluation,
                            EvaluationStatus::Failed,
                            format!("Failed to insert builds: {}", e),
                            Some("db-insert".to_string()),
                        )
                        .await;
                        return;
                    }
                }
            }

            // 5. Derivation features (FK satisfied now).
            for pf in result.pending_features {
                if let Err(e) = gradient_core::db::add_features(
                    Arc::clone(&state),
                    pf.features,
                    Some(pf.derivation_id),
                    None,
                )
                .await
                {
                    error!(error = %e, derivation_id = %pf.derivation_id, "Failed to add features for derivation");
                }
            }

            if let Some(project_id) = evaluation.project {
                let now = chrono::Utc::now().naive_utc();
                let active_entry_points = result.entry_points
                    .iter()
                    .map(|ep| AEntryPoint {
                        id: ActiveValue::Set(Uuid::new_v4()),
                        project: ActiveValue::Set(project_id),
                        evaluation: ActiveValue::Set(evaluation.id),
                        build: ActiveValue::Set(ep.build_id),
                        eval: ActiveValue::Set(ep.wildcard.clone()),
                        created_at: ActiveValue::Set(now),
                    })
                    .collect::<Vec<AEntryPoint>>();

                if !active_entry_points.is_empty() {
                    for chunk in active_entry_points.chunks(BATCH_SIZE) {
                        if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                            .exec(&state.db)
                            .await
                        {
                            error!(error = %e, "Failed to insert entry points");
                        }
                    }
                }

                // Rebuild the (build_id, wildcard) vec for CI reporting.
                let ep_pairs: Vec<(Uuid, String)> = result.entry_points
                    .iter()
                    .map(|ep| (ep.build_id, ep.wildcard.clone()))
                    .collect();
                report_ci_for_entry_points(
                    Arc::clone(&state),
                    project_id,
                    evaluation.commit,
                    &evaluation.repository,
                    evaluation.id,
                    &ep_pairs,
                    CiStatus::Pending,
                )
                .await;
            }

            // Persist per-attr evaluation failures as individual evaluation_message rows.
            if !result.failed_derivations.is_empty() {
                warn!(count = result.failed_derivations.len(), "Partial evaluation failure — some derivations skipped");
                for fd in &result.failed_derivations {
                    record_evaluation_message(
                        &state,
                        evaluation.id,
                        MessageLevel::Error,
                        fd.error.clone(),
                        Some(format!("nix-eval:{}", fd.derivation)),
                    )
                    .await;
                }
            }

            // Persist evaluation warnings (e.g. Nix "evaluation warning: …" messages).
            for warning in &result.warnings {
                record_evaluation_message(
                    &state,
                    evaluation.id,
                    MessageLevel::Warning,
                    warning.clone(),
                    Some("nix-eval".to_string()),
                )
                .await;
            }

            // Transition all Created builds to Queued now that dependency records are fully inserted.
            let created_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(evaluation.id))
                .filter(CBuild::Status.eq(BuildStatus::Created))
                .all(&state.db)
                .await
                .unwrap_or_default();

            if created_builds.is_empty() {
                update_evaluation_status(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Completed,
                )
                .await;
                return;
            }

            for build in created_builds {
                update_build_status(Arc::clone(&state), build, BuildStatus::Queued).await;
            }

            info!("Starting evaluation build phase");
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building)
                .await;
        }

        Err(e) => {
            error!(error = %format!("{:#}", e), "Failed to evaluate");
            // Determine source from the error message prefix set by evaluate().
            let source = {
                let msg = format!("{}", e);
                if msg.contains("prefetch") || msg.contains("fetch") {
                    Some("flake-prefetch".to_string())
                } else {
                    Some("nix-eval".to_string())
                }
            };
            // Report the top-level "gradient" check as Failure — nix eval errored out.
            if let Some(project_id) = evaluation.project {
                report_ci_for_evaluation(
                    Arc::clone(&state),
                    project_id,
                    evaluation.commit,
                    &evaluation.repository,
                    evaluation.id,
                    CiStatus::Failure,
                )
                .await;
            }
            update_evaluation_status_with_error(
                Arc::clone(&state),
                evaluation,
                EvaluationStatus::Failed,
                format!("{}", e),
                source,
            )
            .await;
        }
    }
}
