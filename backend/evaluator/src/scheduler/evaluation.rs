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
        Ok(builds) => {
            // Re-fetch to check if aborted while evaluate() was running
            match EEvaluation::find_by_id(evaluation.id).one(&state.db).await {
                Ok(Some(current)) if current.status == EvaluationStatus::Aborted => return,
                _ => {}
            }

            let (
                builds,
                new_derivations,
                new_derivation_outputs,
                new_derivation_dependencies,
                entry_point_build_ids,
                failed_derivations,
                pending_features,
                eval_warnings,
            ) = builds;

            info!(
                build_count = builds.len(),
                new_derivation_count = new_derivations.len(),
                dependency_count = new_derivation_dependencies.len(),
                "Created builds + derivations"
            );

            const BATCH_SIZE: usize = 1000;

            // 1. Derivations first (builds + derivation_output FK into it).
            if !new_derivations.is_empty() {
                let active: Vec<ADerivation> = new_derivations
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
            if !new_derivation_outputs.is_empty() {
                for chunk in new_derivation_outputs.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivation outputs");
                    }
                }
            }

            // 3. Derivation dependency edges.
            if !new_derivation_dependencies.is_empty() {
                let active: Vec<ADerivationDependency> = new_derivation_dependencies
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
            let active_builds = builds
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
            for (derivation_id, features) in pending_features {
                if let Err(e) = gradient_core::db::add_features(
                    Arc::clone(&state),
                    features,
                    Some(derivation_id),
                    None,
                )
                .await
                {
                    error!(error = %e, %derivation_id, "Failed to add features for derivation");
                }
            }

            if let Some(project_id) = evaluation.project {
                let now = chrono::Utc::now().naive_utc();
                let active_entry_points = entry_point_build_ids
                    .iter()
                    .map(|(build_id, eval)| AEntryPoint {
                        id: ActiveValue::Set(Uuid::new_v4()),
                        project: ActiveValue::Set(project_id),
                        evaluation: ActiveValue::Set(evaluation.id),
                        build: ActiveValue::Set(*build_id),
                        eval: ActiveValue::Set(eval.clone()),
                        created_at: ActiveValue::Set(now),
                    })
                    .collect::<Vec<AEntryPoint>>();

                if !active_entry_points.is_empty() {
                    const BATCH_SIZE: usize = 1000;
                    for chunk in active_entry_points.chunks(BATCH_SIZE) {
                        if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                            .exec(&state.db)
                            .await
                        {
                            error!(error = %e, "Failed to insert entry points");
                        }
                    }
                }

                // Report one CI check per entry point (Pending — builds are now queued).
                report_ci_for_entry_points(
                    Arc::clone(&state),
                    project_id,
                    evaluation.commit,
                    &evaluation.repository,
                    evaluation.id,
                    &entry_point_build_ids,
                    CiStatus::Pending,
                )
                .await;
            }

            // Transition all Created builds for this evaluation to Queued now that
            // their dependency records are fully inserted. This covers both newly
            // created builds and clones of previously-failed builds.
            let created_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(evaluation.id))
                .filter(CBuild::Status.eq(BuildStatus::Created))
                .all(&state.db)
                .await
                .unwrap_or_default();

            // Persist per-attr evaluation failures as individual evaluation_message rows.
            if !failed_derivations.is_empty() {
                warn!(count = failed_derivations.len(), "Partial evaluation failure — some derivations skipped");
                for (attr, err_msg) in &failed_derivations {
                    record_evaluation_message(
                        &state,
                        evaluation.id,
                        MessageLevel::Error,
                        err_msg.clone(),
                        Some(format!("nix-eval:{}", attr)),
                    )
                    .await;
                }
            }

            // Persist evaluation warnings (e.g. Nix "evaluation warning: …" messages).
            if !eval_warnings.is_empty() {
                for warning in &eval_warnings {
                    record_evaluation_message(
                        &state,
                        evaluation.id,
                        MessageLevel::Warning,
                        warning.clone(),
                        Some("nix-eval".to_string()),
                    )
                    .await;
                }
            }

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
