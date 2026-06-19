/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::trigger::TriggerError;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

/// Loads the most recent evaluation for `project_id` together with its entry
/// points. Restart no longer pre-creates per-eval build rows; the new eval
/// re-resolves anchors when it runs, so we only need the entry-point
/// derivations to seed it.
pub(super) async fn previous_evaluation_with_entry_points<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<(MEvaluation, Vec<MEntryPoint>), TriggerError> {
    let prev_eval = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project_id))
        .order_by_desc(CEvaluation::CreatedAt)
        .one(db)
        .await?
        .ok_or(TriggerError::NoPreviousEvaluation)?;

    let prev_entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(prev_eval.id))
        .all(db)
        .await?;

    Ok((prev_eval, prev_entry_points))
}
