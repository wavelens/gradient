/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ci::trigger::TriggerError;
use gradient_types::*;
use chrono::NaiveDateTime;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;

/// Copies the previous evaluation's entry points onto `new_eval_id`, remapping
/// each entry point's build through `build_id_map`. Entry points whose build was
/// not recreated (no map entry) are skipped.
pub(super) async fn copy_entry_points<C: ConnectionTrait>(
    db: &C,
    prev_eval_id: EvaluationId,
    new_eval_id: EvaluationId,
    build_id_map: &HashMap<BuildId, BuildId>,
    now: NaiveDateTime,
) -> Result<(), TriggerError> {
    let prev_entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(prev_eval_id))
        .all(db)
        .await?;

    for prev_ep in prev_entry_points {
        if let Some(&new_build_id) = build_id_map.get(&prev_ep.build) {
            let aep = AEntryPoint {
                id: Set(EntryPointId::now_v7()),
                project: Set(prev_ep.project),
                evaluation: Set(new_eval_id),
                build: Set(new_build_id),
                eval: Set(prev_ep.eval),
                created_at: Set(now),
                repo_check_id: Set(None),
            };
            aep.insert(db).await?;
        }
    }

    Ok(())
}
