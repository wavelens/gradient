/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ci::trigger::TriggerError;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

/// Loads the most recent evaluation for `project_id` together with all of its
/// builds. The builds are read *before* the new evaluation is inserted so the
/// caller can decide the new evaluation's initial status from them.
pub(super) async fn previous_evaluation_with_builds<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<(MEvaluation, Vec<MBuild>), TriggerError> {
    let prev_eval = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project_id))
        .order_by_desc(CEvaluation::CreatedAt)
        .one(db)
        .await?
        .ok_or(TriggerError::NoPreviousEvaluation)?;

    let prev_builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(prev_eval.id))
        .all(db)
        .await?;

    Ok((prev_eval, prev_builds))
}
