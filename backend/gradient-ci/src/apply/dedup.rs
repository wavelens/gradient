/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ApplyInput;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait};

/// Same-commit deduplication. Returns `true` when the trigger should be skipped
/// because the commit is already being (or was just) evaluated. Skips when:
///   - an in-flight evaluation is already running on this commit
///     (covers polling-while-build-is-running, even if `last_evaluation`
///     is dangling or points elsewhere), OR
///   - `last_evaluation`'s commit matches (covers terminal-then-poll-again).
///
/// Time triggers and manual fires bypass the check entirely (returns `false`).
pub(super) async fn skip_for_same_commit<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
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

    if let Some(prev) = project.last_evaluation
        && let Some(prev_eval) = EEvaluation::find_by_id(prev).one(db).await?
        && let Some(prev_commit) = ECommit::find_by_id(prev_eval.commit).one(db).await?
        && prev_commit.hash == input.commit_hash
    {
        return Ok(true);
    }

    Ok(false)
}
