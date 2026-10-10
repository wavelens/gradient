/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod entry_points;

use super::TriggerError;
use super::flake_snapshot::snapshot_flake_input_overrides;
use super::new_evaluation::{ensure_no_active_evaluation, link_to_previous};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
};

/// A restart never walks. The new evaluation is taking over the previous one's names because the
/// graph heal is seeding its thaw from them.
pub async fn trigger_evaluation_retry<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    prev_eval: &MEvaluation,
) -> Result<MEvaluation, TriggerError> {
    ensure_no_active_evaluation(db, task.id).await?;

    let prev_entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(prev_eval.id))
        .all(db)
        .await?;

    let now = gradient_types::now();
    let initial_status = restart_initial_status(db, &prev_entry_points).await?;

    let new_eval_id = EvaluationId::now_v7();
    let aevaluation = MEvaluation {
        id: new_eval_id,
        task: Some(task.id),
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
    link_to_previous(db, &new_eval).await?;

    snapshot_flake_input_overrides(db, task.id, new_eval.id).await?;

    entry_points::copy_entry_points(db, &prev_entry_points, new_eval_id, now).await?;
    if initial_status == EvaluationStatus::Building {
        gradient_db::graph::reachability::inherit_names(db, prev_eval.id, new_eval_id).await?;
    }

    let mut atask: ATask = task.clone().into();
    atask.last_evaluation = Set(Some(new_eval_id));
    atask.update(db).await?;

    Ok(new_eval)
}

/// A missing shared build is pending because the new evaluation must build it.
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

    let shared_builds = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(derivation_ids.clone()))
        .all(db)
        .await?;

    let all_cached = shared_builds.len() == derivation_ids.len()
        && shared_builds
            .iter()
            .all(|a| matches!(a.status, BuildStatus::Completed | BuildStatus::Substituted));

    Ok(if all_cached {
        EvaluationStatus::Completed
    } else {
        EvaluationStatus::Building
    })
}
