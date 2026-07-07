/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::trigger::TriggerError;
use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};

/// Copies the previous evaluation's entry points onto `new_eval_id`, carrying
/// each one's `derivation` straight across. The new eval re-resolves anchors
/// for those derivations when it runs.
pub(super) async fn copy_entry_points<C: ConnectionTrait>(
    db: &C,
    prev_entry_points: &[MEntryPoint],
    new_eval_id: EvaluationId,
    now: NaiveDateTime,
) -> Result<(), TriggerError> {
    for prev_ep in prev_entry_points {
        let aep = MEntryPoint {
            id: EntryPointId::now_v7(),
            project: prev_ep.project,
            evaluation: new_eval_id,
            derivation: prev_ep.derivation,
            eval: prev_ep.eval.clone(),
            created_at: now,
            ..Default::default()
        }
        .into_active_model();

        aep.insert(db).await?;
    }

    Ok(())
}
