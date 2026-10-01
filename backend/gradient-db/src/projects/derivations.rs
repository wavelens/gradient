/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Derivations a project has built. Replaces the dropped per-project
//! `derivation.project` scoping now that derivations are a global graph:
//! ownership is derived through the project's tasks -> evaluations -> build_jobs.

use crate::fetch_in_chunks;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;

/// Distinct derivations referenced by builds in `project_id`'s evaluations.
pub async fn derivation_ids_for_project<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<Vec<DerivationId>, DbErr> {
    let task_ids: Vec<TaskId> = ETask::find()
        .select_only()
        .column(CTask::Id)
        .filter(CTask::Project.eq(project_id))
        .into_tuple::<TaskId>()
        .all(db)
        .await?;
    if task_ids.is_empty() {
        return Ok(vec![]);
    }

    let eval_ids = fetch_in_chunks(&task_ids, |chunk| async move {
        EEvaluation::find()
            .select_only()
            .column(CEvaluation::Id)
            .filter(CEvaluation::Task.is_in(chunk))
            .into_tuple::<EvaluationId>()
            .all(db)
            .await
    })
    .await?;
    if eval_ids.is_empty() {
        return Ok(vec![]);
    }

    let drv_ids = fetch_in_chunks(&eval_ids, |chunk| async move {
        EBuildJob::find()
            .select_only()
            .column(CBuildJob::Derivation)
            .distinct()
            .filter(CBuildJob::Evaluation.is_in(chunk))
            .into_tuple::<DerivationId>()
            .all(db)
            .await
    })
    .await?;

    Ok(drv_ids
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect())
}
