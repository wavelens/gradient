/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn project_role_granted_to_a_team<C: ConnectionTrait>(
    db: &C,
    role: RoleId,
) -> Result<bool, DbErr> {
    Ok(ETeamProject::find()
        .filter(CTeamProject::Role.eq(role))
        .one(db)
        .await?
        .is_some()
        || ETeamProjectRequest::find()
            .filter(CTeamProjectRequest::Role.eq(role))
            .one(db)
            .await?
            .is_some())
}

pub async fn cache_role_granted_to_a_team<C: ConnectionTrait>(
    db: &C,
    role: RoleId,
) -> Result<bool, DbErr> {
    Ok(ETeamCache::find()
        .filter(CTeamCache::Role.eq(role))
        .one(db)
        .await?
        .is_some()
        || ETeamCacheRequest::find()
            .filter(CTeamCacheRequest::Role.eq(role))
            .one(db)
            .await?
            .is_some())
}

pub async fn apply_new_project_grants<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<TeamId>, DbErr> {
    let teams = ETeam::find()
        .filter(
            Condition::any()
                .add(CTeam::NewProjectUsers.eq(true))
                .add(CTeam::NewProjectWorkers.eq(true)),
        )
        .all(db)
        .await?;

    let mut granted = Vec::new();
    for team in teams {
        let users = team.new_project_users && team.new_project_role.is_some();
        if !users && !team.new_project_workers {
            continue;
        }
        ATeamProject {
            id: Set(TeamProjectId::now_v7()),
            team: Set(team.id),
            project: Set(project),
            role: Set(team.new_project_role.filter(|_| users)),
            includes_users: Set(users),
            includes_workers: Set(team.new_project_workers),
            managed: Set(false),
            created_at: Set(gradient_types::now()),
        }
        .insert(db)
        .await?;
        granted.push(team.id);
    }

    Ok(granted)
}

pub async fn project_grant<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    project: ProjectId,
) -> Result<Option<MTeamProject>, DbErr> {
    ETeamProject::find()
        .filter(CTeamProject::Team.eq(team))
        .filter(CTeamProject::Project.eq(project))
        .one(db)
        .await
}

pub async fn grants_of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MTeamProject>, DbErr> {
    ETeamProject::find()
        .filter(CTeamProject::Project.eq(project))
        .all(db)
        .await
}

pub async fn project_grants_of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamProject>, DbErr> {
    ETeamProject::find()
        .filter(CTeamProject::Team.eq(team))
        .all(db)
        .await
}

pub async fn grant_project<C: ConnectionTrait>(
    db: &C,
    grant: MTeamProject,
) -> Result<MTeamProject, DbErr> {
    grant.into_active_model().insert(db).await
}

pub async fn update_project_grant<C: ConnectionTrait>(
    db: &C,
    grant: ATeamProject,
) -> Result<MTeamProject, DbErr> {
    grant.update(db).await
}

pub async fn revoke_project_grant<C: ConnectionTrait>(
    db: &C,
    grant: MTeamProject,
) -> Result<(), DbErr> {
    grant.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn cache_grant<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    cache: CacheId,
) -> Result<Option<MTeamCache>, DbErr> {
    ETeamCache::find()
        .filter(CTeamCache::Team.eq(team))
        .filter(CTeamCache::Cache.eq(cache))
        .one(db)
        .await
}

pub async fn grants_of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<MTeamCache>, DbErr> {
    ETeamCache::find()
        .filter(CTeamCache::Cache.eq(cache))
        .all(db)
        .await
}

pub async fn cache_grants_of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamCache>, DbErr> {
    ETeamCache::find()
        .filter(CTeamCache::Team.eq(team))
        .all(db)
        .await
}

pub async fn grant_cache<C: ConnectionTrait>(
    db: &C,
    grant: MTeamCache,
) -> Result<MTeamCache, DbErr> {
    grant.into_active_model().insert(db).await
}

pub async fn update_cache_grant<C: ConnectionTrait>(
    db: &C,
    grant: ATeamCache,
) -> Result<MTeamCache, DbErr> {
    grant.update(db).await
}

pub async fn revoke_cache_grant<C: ConnectionTrait>(
    db: &C,
    grant: MTeamCache,
) -> Result<(), DbErr> {
    grant.into_active_model().delete(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use gradient_types::consts::BASE_ROLE_WRITE_ID;
    use sea_orm::MockExecResult;

    #[tokio::test]
    async fn new_projects_get_the_grants_their_teams_ask_for() {
        let granting = MTeam {
            id: TeamId::now_v7(),
            new_project_users: true,
            new_project_workers: true,
            new_project_role: Some(BASE_ROLE_WRITE_ID),
            ..Default::default()
        };
        let users_without_role = MTeam {
            id: TeamId::now_v7(),
            new_project_users: true,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![granting.clone(), users_without_role]])
            .append_query_results([vec![MTeamProject {
                team: granting.id,
                includes_users: true,
                includes_workers: true,
                role: Some(BASE_ROLE_WRITE_ID),
                ..Default::default()
            }]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let granted = apply_new_project_grants(&db, ProjectId::now_v7())
            .await
            .expect("grants");

        assert_eq!(granted, vec![granting.id]);
    }
}
