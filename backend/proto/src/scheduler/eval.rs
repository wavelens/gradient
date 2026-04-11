/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `EvalResult` messages from workers.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::evaluation_message::MessageLevel;
use gradient_core::db::{
    record_evaluation_message, update_build_status, update_evaluation_status,
    update_evaluation_status_with_error,
};
use gradient_core::sources::get_hash_from_path;
use gradient_core::types::*;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use tracing::{error, info};
use uuid::Uuid;

use crate::messages::{Architecture as ProtoArch, DiscoveredDerivation};
use super::jobs::PendingEvalJob;

const BATCH_SIZE: usize = 1000;

pub async fn handle_eval_result(
    state: &Arc<ServerState>,
    job: &PendingEvalJob,
    derivations: Vec<DiscoveredDerivation>,
    warnings: Vec<String>,
) -> Result<()> {
    let evaluation_id = job.evaluation_id;
    let organization_id = job.organization_id;

    let current = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .context("fetch evaluation")?;
    let evaluation = match current {
        Some(e) if e.status == EvaluationStatus::Aborted => {
            info!(%evaluation_id, "evaluation aborted; discarding worker result");
            return Ok(());
        }
        Some(e) => e,
        None => anyhow::bail!("evaluation {} not found", evaluation_id),
    };

    info!(
        %evaluation_id,
        derivation_count = derivations.len(),
        warning_count = warnings.len(),
        "processing eval result from worker",
    );

    // Load existing derivations to avoid re-inserting.
    let existing_paths: Vec<String> = derivations.iter().map(|d| d.drv_path.clone()).collect();
    let existing: Vec<MDerivation> = if !existing_paths.is_empty() {
        EDerivation::find()
            .filter(CDerivation::Organization.eq(organization_id))
            .filter(CDerivation::DerivationPath.is_in(existing_paths))
            .all(&state.db)
            .await
            .context("query existing derivations")?
    } else {
        vec![]
    };

    let mut drv_path_to_id: HashMap<String, Uuid> =
        existing.iter().map(|d| (d.derivation_path.clone(), d.id)).collect();

    // Insert new derivation rows.
    let now = chrono::Utc::now().naive_utc();
    let mut new_derivations: Vec<ADerivation> = Vec::new();
    let mut new_outputs: Vec<ADerivationOutput> = Vec::new();

    for d in &derivations {
        if drv_path_to_id.contains_key(&d.drv_path) {
            continue;
        }
        let id = Uuid::new_v4();
        drv_path_to_id.insert(d.drv_path.clone(), id);
        new_derivations.push(ADerivation {
            id: Set(id),
            organization: Set(organization_id),
            derivation_path: Set(d.drv_path.clone()),
            architecture: Set(proto_arch_to_entity(&d.architecture)),
            created_at: Set(now),
        });
        for output in &d.outputs {
            let (hash, package) = get_hash_from_path(output.path.clone())
                .unwrap_or_else(|_| ("unknown".to_owned(), output.name.clone()));
            new_outputs.push(ADerivationOutput {
                id: Set(Uuid::new_v4()),
                derivation: Set(id),
                name: Set(output.name.clone()),
                output: Set(output.path.clone()),
                hash: Set(hash),
                package: Set(package),
                ca: Set(None),
                file_hash: Set(None),
                file_size: Set(None),
                nar_size: Set(None),
                is_cached: Set(false),
                has_artefacts: Set(false),
                created_at: Set(now),
            });
        }
    }

    if !new_derivations.is_empty() {
        for chunk in new_derivations.chunks(BATCH_SIZE) {
            if let Err(e) = EDerivation::insert_many(chunk.to_vec()).exec(&state.db).await {
                error!(error = %e, "failed to insert derivations");
                update_evaluation_status_with_error(
                    Arc::clone(state),
                    evaluation.clone(),
                    EvaluationStatus::Failed,
                    format!("failed to insert derivations: {}", e),
                    Some("db-insert".to_string()),
                )
                .await;
                return Err(e.into());
            }
        }
    }
    if !new_outputs.is_empty() {
        for chunk in new_outputs.chunks(BATCH_SIZE) {
            if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec()).exec(&state.db).await {
                error!(error = %e, "failed to insert derivation outputs");
            }
        }
    }

    // Dependency edges.
    let mut dep_edges: Vec<ADerivationDependency> = Vec::new();
    for d in &derivations {
        let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else { continue };
        for dep_path in &d.dependencies {
            if let Some(&dep_id) = drv_path_to_id.get(dep_path) {
                dep_edges.push(ADerivationDependency {
                    id: Set(Uuid::new_v4()),
                    derivation: Set(drv_id),
                    dependency: Set(dep_id),
                });
            }
        }
    }
    if !dep_edges.is_empty() {
        for chunk in dep_edges.chunks(BATCH_SIZE) {
            if let Err(e) = EDerivationDependency::insert_many(chunk.to_vec()).exec(&state.db).await {
                error!(error = %e, "failed to insert dependency edges");
            }
        }
    }

    // Build rows.
    let mut builds: Vec<ABuild> = Vec::new();
    for d in &derivations {
        let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else { continue };
        let status = if d.substituted { BuildStatus::Substituted } else { BuildStatus::Created };
        builds.push(ABuild {
            id: Set(Uuid::new_v4()),
            evaluation: Set(evaluation_id),
            derivation: Set(drv_id),
            status: Set(status),
            server: Set(None),
            log_id: Set(None),
            build_time_ms: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        });
    }
    if !builds.is_empty() {
        for chunk in builds.chunks(BATCH_SIZE) {
            if let Err(e) = EBuild::insert_many(chunk.to_vec()).exec(&state.db).await {
                error!(error = %e, "failed to insert builds");
                update_evaluation_status_with_error(
                    Arc::clone(state),
                    evaluation.clone(),
                    EvaluationStatus::Failed,
                    format!("failed to insert builds: {}", e),
                    Some("db-insert".to_string()),
                )
                .await;
                return Err(e.into());
            }
        }
    }

    // System features.
    for d in &derivations {
        if d.required_features.is_empty() { continue; }
        let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else { continue };
        if let Err(e) = gradient_core::db::add_features(
            Arc::clone(state),
            d.required_features.clone(),
            Some(drv_id),
            None,
        ).await {
            error!(error = %e, %drv_id, "failed to add system features");
        }
    }

    // Warnings.
    for warning in &warnings {
        record_evaluation_message(state, evaluation_id, MessageLevel::Warning, warning.clone(), Some("nix-eval".to_string())).await;
    }

    // Transition Created → Queued.
    let created = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.eq(BuildStatus::Created))
        .all(&state.db)
        .await
        .unwrap_or_default();

    if created.is_empty() {
        update_evaluation_status(Arc::clone(state), evaluation, EvaluationStatus::Completed).await;
        return Ok(());
    }

    for build in created {
        update_build_status(Arc::clone(state), build, BuildStatus::Queued).await;
    }

    info!(%evaluation_id, "eval result processed; builds queued");
    update_evaluation_status(Arc::clone(state), evaluation, EvaluationStatus::Building).await;
    Ok(())
}

pub async fn handle_eval_job_completed(state: &Arc<ServerState>, evaluation_id: Uuid) -> Result<()> {
    let active = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.is_in(vec![BuildStatus::Created, BuildStatus::Queued, BuildStatus::Building]))
        .all(&state.db).await.unwrap_or_default();

    if active.is_empty() {
        if let Some(eval) = EEvaluation::find_by_id(evaluation_id).one(&state.db).await? {
            if eval.status == EvaluationStatus::Building {
                update_evaluation_status(Arc::clone(state), eval, EvaluationStatus::Completed).await;
            }
        }
    }
    Ok(())
}

pub async fn handle_eval_job_failed(state: &Arc<ServerState>, evaluation_id: Uuid, error: &str) -> Result<()> {
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id).one(&state.db).await? {
        if !matches!(eval.status, EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted) {
            update_evaluation_status_with_error(
                Arc::clone(state),
                eval,
                EvaluationStatus::Failed,
                error.to_owned(),
                Some("worker".to_string()),
            ).await;
        }
    }
    Ok(())
}

fn proto_arch_to_entity(arch: &ProtoArch) -> entity::server::Architecture {
    match arch {
        ProtoArch::Builtin        => entity::server::Architecture::BUILTIN,
        ProtoArch::X86_64Linux    => entity::server::Architecture::X86_64Linux,
        ProtoArch::Aarch64Linux   => entity::server::Architecture::Aarch64Linux,
        ProtoArch::X86_64Darwin   => entity::server::Architecture::X86_64Darwin,
        ProtoArch::Aarch64Darwin  => entity::server::Architecture::Aarch64Darwin,
    }
}
