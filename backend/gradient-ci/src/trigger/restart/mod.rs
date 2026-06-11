/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod build_mapping;
mod entry_points;
mod previous_lookup;

use super::TriggerError;
use super::flake_snapshot::snapshot_flake_input_overrides;
use super::new_evaluation::ensure_no_active_evaluation;
use gradient_types::*;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};

/// Status mapping applied to each previous build when restarting.
///
/// Outputs already present in the cache are marked `Substituted` so the worker
/// skips them; everything else is re-queued for a fresh build.
pub(crate) fn restart_build_status(prev: BuildStatus) -> BuildStatus {
    match prev {
        BuildStatus::Completed | BuildStatus::Substituted => BuildStatus::Substituted,
        _ => BuildStatus::Queued,
    }
}

/// Creates a new `Building` evaluation that skips the fetch+eval phase and
/// re-runs only the failed builds from the most recent evaluation.
///
/// Status mapping from the previous build:
/// - `Completed` | `Substituted` → `Substituted`  (already in the cache; no rebuild needed)
/// - everything else             → `Queued`        (rebuild)
///
/// Entry points are copied from the previous evaluation and linked to the new builds.
/// The scheduler's build-dispatch loop will pick up the `Queued` builds on its next tick.
pub async fn trigger_restart_builds<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
) -> Result<MEvaluation, TriggerError> {
    ensure_no_active_evaluation(db, project.id).await?;

    let (prev_eval, prev_builds) =
        previous_lookup::previous_evaluation_with_builds(db, project.id).await?;

    let now = gradient_types::now();

    // Decide the new evaluation's initial status from the previous builds. If
    // every previous build maps to `Substituted` (nothing to actually rebuild),
    // the evaluation is inserted as `Completed`; otherwise it starts in
    // `Building` and the scheduler's `check_evaluation_done` closes it out as
    // the queued builds finish.
    //
    // Without this, an all-`Substituted` restart would leave the evaluation
    // stuck in `Building` forever - no build job ever runs, so nothing fires
    // the completion check.
    let any_pending = prev_builds
        .iter()
        .any(|b| !matches!(restart_build_status(b.status), BuildStatus::Substituted));
    let initial_status = if any_pending {
        EvaluationStatus::Building
    } else {
        EvaluationStatus::Completed
    };

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

    let build_id_map =
        build_mapping::create_restart_builds(db, project, new_eval_id, &prev_builds, now).await?;

    entry_points::copy_entry_points(db, prev_eval.id, new_eval_id, &build_id_map, now).await?;

    let mut aproject: AProject = project.clone().into();
    aproject.last_evaluation = Set(Some(new_eval_id));
    aproject.update(db).await?;

    Ok(new_eval)
}
