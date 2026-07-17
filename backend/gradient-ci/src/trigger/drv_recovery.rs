/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::TriggerError;
use super::flake_snapshot::snapshot_flake_input_overrides;
use crate::abort::{AbortKind, abort_evaluation};
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};

/// Auto-recover an evaluation wedged in `graph_stuck` because an anchor's own
/// `.drv` NAR is missing from our cache and has no producer: only evaluation
/// emits a `.drv`, and the daemon-free server cannot reproduce one. Aborts the
/// stuck run (freeing the single-active-per-project slot) and queues a fresh
/// full evaluation of the **same commit** - re-instantiating the flake
/// re-materialises the `.drv` in the worker store, which re-uploads it.
///
/// The new run is [`EvaluationKind::DrvRecovery`] so the caller can make the
/// recovery one-shot: a `.drv` a cold re-eval still fails to persist is not a
/// transient miss, and re-triggering again would loop.
pub async fn trigger_drv_recovery<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    stuck: &MEvaluation,
) -> Result<MEvaluation, TriggerError> {
    // Abort before insert: the single-active-per-project unique index rejects a
    // second active row, so the stuck eval must leave the active set first.
    abort_evaluation(db, stuck.id, AbortKind::Soft).await?;

    let now = gradient_types::now();
    let new_eval_id = EvaluationId::now_v7();
    let aevaluation = MEvaluation {
        id: new_eval_id,
        project: Some(project.id),
        repository: stuck.repository.clone(),
        commit: stuck.commit,
        wildcard: stuck.wildcard.clone(),
        status: EvaluationStatus::Queued,
        previous: Some(stuck.id),
        kind: EvaluationKind::DrvRecovery,
        created_at: now,
        updated_at: now,
        flake_source: stuck.flake_source.clone(),
        ..Default::default()
    }
    .into_active_model();

    let new_eval = aevaluation.insert(db).await?;

    snapshot_flake_input_overrides(db, project.id, new_eval.id).await?;

    let mut aproject: AProject = project.clone().into();
    aproject.last_evaluation = Set(Some(new_eval_id));
    aproject.update(db).await?;

    Ok(new_eval)
}
