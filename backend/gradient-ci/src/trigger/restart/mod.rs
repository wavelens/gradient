/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod entry_points;
mod previous_lookup;

use super::TriggerError;
use super::flake_snapshot::snapshot_flake_input_overrides;
use super::new_evaluation::ensure_no_active_evaluation;
use gradient_types::*;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
};

/// Creates a new evaluation that re-runs the previous evaluation's entry points.
///
/// Builds are no longer pre-created per eval: the global `derivation_build`
/// anchors carry build state, and the new eval re-resolves them when it runs.
/// The initial status is derived from the previous entry-point anchors: if every
/// one is already terminal-success (`Completed`/`Substituted`) there is nothing
/// to rebuild and the eval starts `Completed`; otherwise it starts `Building`
/// and the scheduler's `check_evaluation_done` closes it out.
pub async fn trigger_restart_builds<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
) -> Result<MEvaluation, TriggerError> {
    ensure_no_active_evaluation(db, project.id).await?;

    let (prev_eval, prev_entry_points) =
        previous_lookup::previous_evaluation_with_entry_points(db, project.id).await?;

    let now = gradient_types::now();
    let initial_status = restart_initial_status(db, &prev_entry_points).await?;

    let new_eval_id = EvaluationId::now_v7();
    let aevaluation = MEvaluation {
        id: new_eval_id,
        project: Some(project.id),
        repository: prev_eval.repository.clone(),
        commit: prev_eval.commit,
        wildcard: prev_eval.wildcard.clone(),
        status: initial_status,
        previous: Some(prev_eval.id),
        created_at: now,
        updated_at: now,
        flake_source: prev_eval.flake_source.clone(),
        ..Default::default()
    }
    .into_active_model();

    let new_eval = aevaluation.insert(db).await?;

    snapshot_flake_input_overrides(db, project.id, new_eval.id).await?;

    entry_points::copy_entry_points(db, &prev_entry_points, new_eval_id, now).await?;

    let mut aproject: AProject = project.clone().into();
    aproject.last_evaluation = Set(Some(new_eval_id));
    aproject.update(db).await?;

    Ok(new_eval)
}

/// `Completed` when every entry-point anchor is already terminal-success,
/// otherwise `Building`. An anchor missing entirely counts as pending: the new
/// eval must run to (re)build it.
async fn restart_initial_status<C: ConnectionTrait>(
    db: &C,
    prev_entry_points: &[MEntryPoint],
) -> Result<EvaluationStatus, TriggerError> {
    let derivation_ids: Vec<DerivationId> = prev_entry_points
        .iter()
        .map(|ep| ep.derivation)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    if derivation_ids.is_empty() {
        return Ok(EvaluationStatus::Completed);
    }

    let anchors = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(derivation_ids.clone()))
        .all(db)
        .await?;

    let all_cached = anchors.len() == derivation_ids.len()
        && anchors.iter().all(|a| {
            matches!(
                a.status,
                BuildStatus::Completed | BuildStatus::Substituted
            )
        });

    Ok(if all_cached {
        EvaluationStatus::Completed
    } else {
        EvaluationStatus::Building
    })
}
