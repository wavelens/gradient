/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod grants;
pub mod members;
pub mod workers;

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn team_role_of<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<Option<TeamRole>, DbErr> {
    Ok(ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .one(db)
        .await?
        .map(|membership| membership.role))
}

pub async fn team_worker_ids<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<String>, DbErr> {
    Ok(ETeamWorker::find()
        .filter(CTeamWorker::Team.eq(team))
        .all(db)
        .await?
        .into_iter()
        .map(|worker| worker.worker_id)
        .collect())
}
