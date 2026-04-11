/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `BuildOutput` messages from workers and build job lifecycle.

use std::sync::Arc;

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::db::{update_build_status, update_evaluation_status};
use gradient_core::types::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::messages::BuildOutput;
use super::jobs::PendingBuildJob;

pub async fn handle_build_output(
    state: &Arc<ServerState>,
    _job: &PendingBuildJob,
    build_id: Uuid,
    outputs: Vec<BuildOutput>,
) -> Result<()> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db).await.context("fetch build")?
        .with_context(|| format!("build {} not found", build_id))?;

    let derivation_id = build.derivation;

    for output in &outputs {
        let existing = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(derivation_id))
            .filter(CDerivationOutput::Name.eq(&output.name))
            .one(&state.db).await.context("fetch derivation_output")?;

        if let Some(row) = existing {
            let mut active = row.into_active_model();
            if let Some(nar_size) = output.nar_size {
                active.nar_size = Set(Some(nar_size));
            }
            if let Some(ref nar_hash) = output.nar_hash {
                active.file_hash = Set(Some(nar_hash.clone()));
            }
            active.has_artefacts = Set(output.has_artefacts);
            if let Err(e) = active.update(&state.db).await {
                error!(error = %e, %build_id, output_name = %output.name, "failed to update derivation_output");
            }
        } else {
            warn!(%build_id, output_name = %output.name, "derivation_output row not found");
        }
    }

    info!(%build_id, output_count = outputs.len(), "build outputs recorded");
    Ok(())
}

pub async fn handle_build_job_completed(state: &Arc<ServerState>, build_id: Uuid) -> Result<()> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await? {
        Some(b) => b,
        None => { warn!(%build_id, "build not found on job_completed"); return Ok(()); }
    };
    let evaluation_id = build.evaluation;
    update_build_status(Arc::clone(state), build, BuildStatus::Completed).await;
    check_evaluation_done(state, evaluation_id).await
}

pub async fn handle_build_job_failed(state: &Arc<ServerState>, build_id: Uuid, _error: &str) -> Result<()> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await? {
        Some(b) => b,
        None => { warn!(%build_id, "build not found on job_failed"); return Ok(()); }
    };
    let evaluation_id = build.evaluation;
    let derivation_id = build.derivation;
    update_build_status(Arc::clone(state), build, BuildStatus::Failed).await;
    cascade_dependency_failed(state, evaluation_id, derivation_id).await?;
    check_evaluation_done(state, evaluation_id).await
}

async fn cascade_dependency_failed(
    state: &Arc<ServerState>,
    evaluation_id: Uuid,
    failed_derivation_id: Uuid,
) -> Result<()> {
    let dependents = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.is_in(vec![BuildStatus::Created, BuildStatus::Queued]))
        .all(&state.db).await.context("fetch builds for cascade")?;

    for build in dependents {
        let dep_edge = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.eq(build.derivation))
            .filter(CDerivationDependency::Dependency.eq(failed_derivation_id))
            .one(&state.db).await?;
        if dep_edge.is_some() {
            update_build_status(Arc::clone(state), build, BuildStatus::DependencyFailed).await;
        }
    }
    Ok(())
}

async fn check_evaluation_done(state: &Arc<ServerState>, evaluation_id: Uuid) -> Result<()> {
    let active = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.is_in(vec![BuildStatus::Created, BuildStatus::Queued, BuildStatus::Building]))
        .all(&state.db).await.context("fetch active builds")?;

    if !active.is_empty() { return Ok(()); }

    let Some(eval) = EEvaluation::find_by_id(evaluation_id).one(&state.db).await? else {
        return Ok(());
    };

    if !matches!(eval.status, EvaluationStatus::Building) { return Ok(()); }

    let failed = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.is_in(vec![BuildStatus::Failed, BuildStatus::DependencyFailed]))
        .all(&state.db).await.context("fetch failed builds")?;

    let target = if failed.is_empty() { EvaluationStatus::Completed } else { EvaluationStatus::Failed };
    info!(%evaluation_id, ?target, "evaluation finished");
    update_evaluation_status(Arc::clone(state), eval, target).await;
    Ok(())
}
