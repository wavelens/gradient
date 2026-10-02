/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ApplyInput;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait};

/// The in-flight check is catching a poll during a running build, even with a dangling
/// `last_evaluation`. Time triggers and manual fires are bypassing the check.
pub(super) async fn skip_for_same_commit<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    input: &ApplyInput,
    in_flight: Option<&MEvaluation>,
) -> Result<bool, sea_orm::DbErr> {
    let dedup_applies = !input.manual && input.trigger_type != TriggerType::Time;
    if !dedup_applies {
        return Ok(false);
    }

    if let Some(running) = in_flight
        && let Some(running_commit) = ECommit::find_by_id(running.commit).one(db).await?
        && running_commit.hash == input.commit_hash
    {
        return Ok(true);
    }

    if let Some(prev) = task.last_evaluation
        && let Some(prev_eval) = EEvaluation::find_by_id(prev).one(db).await?
        && let Some(prev_commit) = ECommit::find_by_id(prev_eval.commit).one(db).await?
        && prev_commit.hash == input.commit_hash
    {
        return Ok(true);
    }

    Ok(false)
}
