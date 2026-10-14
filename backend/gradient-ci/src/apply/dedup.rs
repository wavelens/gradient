/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ApplyInput;
use gradient_db::lookup::by_id;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::ConnectionTrait;

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
        && let Some(running_commit) = by_id::<ECommit, _>(db, running.commit).await?
        && running_commit.hash == input.commit_hash
    {
        return Ok(true);
    }

    if let Some(newest) =
        gradient_db::evaluations::branch::newest_on_branch(db, task.id, &input.branch).await?
        && let Some(newest_commit) = by_id::<ECommit, _>(db, newest.commit).await?
        && newest_commit.hash == input.commit_hash
    {
        return Ok(true);
    }

    Ok(false)
}
