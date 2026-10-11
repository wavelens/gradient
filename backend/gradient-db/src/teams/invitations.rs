/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, JoinType,
    QueryFilter, QuerySelect, RelationTrait,
};

pub async fn of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<(MTeamInvitation, Option<MUser>)>, DbErr> {
    ETeamInvitation::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_invitation::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CTeamInvitation::Team.eq(team))
        .all(db)
        .await
}

pub async fn by_team_and_user<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<Option<MTeamInvitation>, DbErr> {
    ETeamInvitation::find()
        .filter(CTeamInvitation::Team.eq(team))
        .filter(CTeamInvitation::User.eq(user))
        .one(db)
        .await
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    invitation: MTeamInvitation,
) -> Result<MTeamInvitation, DbErr> {
    invitation.into_active_model().insert(db).await
}

pub async fn delete<C: ConnectionTrait>(db: &C, invitation: MTeamInvitation) -> Result<(), DbErr> {
    invitation.into_active_model().delete(db).await?;
    Ok(())
}
