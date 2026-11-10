/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod demand;
pub mod grants;
pub mod invitations;
pub mod mail;
pub mod members;
pub mod requests;
pub mod workers;

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

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

#[derive(Debug, Clone, PartialEq)]
pub struct TeamWithMemberAndWorkerCountsAndUserRole {
    pub team: MTeam,
    pub member_count: u64,
    pub worker_count: u64,
    pub user_role: Option<TeamRole>,
}

async fn count_rows_per_team<E, C>(db: &C, team: E::Column) -> Result<HashMap<TeamId, u64>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    Ok(E::find()
        .select_only()
        .column(team)
        .column_as(team.count(), "count")
        .group_by(team)
        .into_tuple::<(TeamId, i64)>()
        .all(db)
        .await?
        .into_iter()
        .map(|(team, count)| (team, count.unsigned_abs()))
        .collect())
}

pub async fn all_teams_with_counts_and_role_of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<TeamWithMemberAndWorkerCountsAndUserRole>, DbErr> {
    let members = count_rows_per_team::<ETeamUser, _>(db, CTeamUser::Team).await?;
    let workers = count_rows_per_team::<ETeamWorker, _>(db, CTeamWorker::Team).await?;
    let roles: HashMap<TeamId, TeamRole> = ETeamUser::find()
        .filter(CTeamUser::User.eq(user))
        .all(db)
        .await?
        .into_iter()
        .map(|membership| (membership.team, membership.role))
        .collect();

    Ok(ETeam::find()
        .order_by_asc(CTeam::DisplayName)
        .order_by_asc(CTeam::Name)
        .all(db)
        .await?
        .into_iter()
        .map(|team| TeamWithMemberAndWorkerCountsAndUserRole {
            member_count: members.get(&team.id).copied().unwrap_or_default(),
            worker_count: workers.get(&team.id).copied().unwrap_or_default(),
            user_role: roles.get(&team.id).copied(),
            team,
        })
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn all_teams_list_includes_member_count_worker_count_and_asking_user_role() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let asking = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the asking user is written");
        let other = crate::rows::insert::<EUser, _>(
            &conn,
            MUser {
                id: UserId::now_v7(),
                username: "other".into(),
                email: "other@example.com".into(),
                ..rows::user()
            },
        )
        .await
        .expect("the other user is written");
        let joined = crate::rows::insert::<ETeam, _>(&conn, rows::team())
            .await
            .expect("the joined team is written");
        let foreign = crate::rows::insert::<ETeam, _>(
            &conn,
            MTeam {
                id: TeamId::now_v7(),
                name: "foreign".into(),
                display_name: "Foreign".into(),
                ..rows::team()
            },
        )
        .await
        .expect("the foreign team is written");
        for (team, user, role) in [
            (joined.id, asking.id, TeamRole::Admin),
            (joined.id, other.id, TeamRole::Member),
            (foreign.id, other.id, TeamRole::Admin),
        ] {
            crate::rows::insert::<ETeamUser, _>(
                &conn,
                MTeamUser {
                    id: TeamUserId::now_v7(),
                    team,
                    user,
                    role,
                    ..Default::default()
                },
            )
            .await
            .expect("the membership is written");
        }
        crate::rows::insert::<ETeamWorker, _>(
            &conn,
            MTeamWorker {
                id: TeamWorkerId::now_v7(),
                team: foreign.id,
                worker_id: "builder".into(),
                created_at: rows::team().created_at,
                ..Default::default()
            },
        )
        .await
        .expect("the worker is written");

        let rows = all_teams_with_counts_and_role_of_user(&conn, asking.id)
            .await
            .expect("the teams are listed");

        assert_eq!(
            rows.iter()
                .map(|row| (
                    row.team.name.as_str(),
                    row.member_count,
                    row.worker_count,
                    row.user_role
                ))
                .collect::<Vec<_>>(),
            [
                ("foreign", 1, 1, None),
                ("platform", 2, 0, Some(TeamRole::Admin))
            ]
        );
        db.finish().await;
    }
}
