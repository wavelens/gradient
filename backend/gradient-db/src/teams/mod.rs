/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod evaluations;
pub mod grants;
pub mod invitations;
pub mod mail;
pub mod members;
pub mod requests;
pub mod workers;

use gradient_types::*;
use sea_orm::sea_query::LockType;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QuerySelect,
};

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

pub async fn by_name<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<MTeam>, DbErr> {
    ETeam::find().filter(CTeam::Name.eq(name)).one(db).await
}

pub async fn lock_for_admin_change<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Option<MTeam>, DbErr> {
    ETeam::find_by_id(team)
        .lock(LockType::NoKeyUpdate)
        .one(db)
        .await
}

pub async fn create<C: ConnectionTrait>(db: &C, team: MTeam) -> Result<MTeam, DbErr> {
    team.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(db: &C, team: ATeam) -> Result<MTeam, DbErr> {
    team.update(db).await
}

pub async fn delete<C: ConnectionTrait>(db: &C, team: MTeam) -> Result<(), DbErr> {
    team.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn by_scim_group<C: ConnectionTrait>(
    db: &C,
    group: &str,
) -> Result<Option<MTeam>, DbErr> {
    ETeam::find()
        .filter(CTeam::ScimGroup.eq(group))
        .one(db)
        .await
}

pub async fn linked_to_scim_group<C: ConnectionTrait>(
    db: &C,
    group: Option<&str>,
) -> Result<Vec<MTeam>, DbErr> {
    let filter = match group {
        Some(group) => CTeam::ScimGroup.eq(group),
        None => CTeam::ScimGroup.is_not_null(),
    };
    ETeam::find().filter(filter).all(db).await
}
