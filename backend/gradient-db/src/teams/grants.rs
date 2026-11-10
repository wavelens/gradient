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

#[derive(Debug, Default, PartialEq)]
pub struct OwnerTeamGrantChange {
    pub added: Option<MTeamProject>,
    pub removed: Vec<MTeamProject>,
}

pub async fn upsert_project_owner_team_grant<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    owner: Option<TeamId>,
) -> Result<OwnerTeamGrantChange, DbErr> {
    let mut stale = ETeamProject::find()
        .filter(CTeamProject::Project.eq(project))
        .filter(CTeamProject::GrantSource.eq(TeamGrantSource::Owner));
    if let Some(owner) = owner {
        stale = stale.filter(CTeamProject::Team.ne(owner));
    }
    let removed = stale.all(db).await?;
    if !removed.is_empty() {
        ETeamProject::delete_many()
            .filter(CTeamProject::Id.is_in(removed.iter().map(|grant| grant.id)))
            .exec(db)
            .await?;
    }

    let Some(owner) = owner else {
        return Ok(OwnerTeamGrantChange {
            added: None,
            removed,
        });
    };
    let Some(team) = ETeam::find_by_id(owner).one(db).await? else {
        return Ok(OwnerTeamGrantChange {
            added: None,
            removed,
        });
    };
    let users = team.new_project_users && team.new_project_role.is_some();
    if (!users && !team.new_project_workers) || project_grant(db, owner, project).await?.is_some() {
        return Ok(OwnerTeamGrantChange {
            added: None,
            removed,
        });
    }

    let added = ATeamProject {
        id: Set(TeamProjectId::now_v7()),
        team: Set(owner),
        project: Set(project),
        role: Set(team.new_project_role.filter(|_| users)),
        includes_users: Set(users),
        includes_workers: Set(team.new_project_workers),
        grant_source: Set(TeamGrantSource::Owner),
        created_at: Set(gradient_types::now()),
    }
    .insert(db)
    .await?;
    Ok(OwnerTeamGrantChange {
        added: Some(added),
        removed,
    })
}

pub async fn insert_worker_grants_for_teams_that_grant_workers_on_every_new_project<
    C: ConnectionTrait,
>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MTeamProject>, DbErr> {
    let granted: Vec<TeamId> = grants_of_project(db, project)
        .await?
        .into_iter()
        .map(|grant| grant.team)
        .collect();
    let teams = ETeam::find()
        .filter(CTeam::NewProjectWorkers.eq(true))
        .filter(CTeam::Id.is_not_in(granted))
        .all(db)
        .await?;
    let mut inserted = Vec::with_capacity(teams.len());
    for team in teams {
        inserted.push(
            ATeamProject {
                id: Set(TeamProjectId::now_v7()),
                team: Set(team.id),
                project: Set(project),
                role: Set(None),
                includes_users: Set(false),
                includes_workers: Set(true),
                grant_source: Set(TeamGrantSource::EveryProjectWorkerGrant),
                created_at: Set(gradient_types::now()),
            }
            .insert(db)
            .await?,
        );
    }
    Ok(inserted)
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
    ETeamProjectRequest::delete_many()
        .filter(CTeamProjectRequest::Team.eq(grant.team))
        .filter(CTeamProjectRequest::Project.eq(grant.project))
        .exec(db)
        .await?;
    grant.into_active_model().insert(db).await
}

pub async fn update_project_grant<C: ConnectionTrait>(
    db: &C,
    grant: ATeamProject,
    read: &MTeamProject,
) -> Result<Option<MTeamProject>, DbErr> {
    let role = match read.role {
        Some(role) => CTeamProject::Role.eq(role),
        None => CTeamProject::Role.is_null(),
    };
    crate::rows::update_where::<ETeamProject, _>(
        db,
        grant,
        Condition::all()
            .add(CTeamProject::IncludesUsers.eq(read.includes_users))
            .add(CTeamProject::IncludesWorkers.eq(read.includes_workers))
            .add(role),
    )
    .await
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
    ETeamCacheRequest::delete_many()
        .filter(CTeamCacheRequest::Team.eq(grant.team))
        .filter(CTeamCacheRequest::Cache.eq(grant.cache))
        .exec(db)
        .await?;
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
    async fn a_grant_replaces_the_pending_request_of_its_team() {
        let grant = MTeamProject {
            team: TeamId::now_v7(),
            project: ProjectId::now_v7(),
            includes_workers: true,
            ..Default::default()
        };
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![grant.clone()]])
            .into_connection();

        grant_project(&db, grant).await.expect("grant");

        let log = gradient_fixtures::mock::raw_statements(db.into_transaction_log());
        assert!(
            log[0]
                .sql
                .starts_with("DELETE FROM \"team_project_request\"")
        );
        assert!(log[1].sql.starts_with("INSERT INTO \"team_project\""));
    }

    async fn two_teams_and_a_project(
        db: &gradient_fixtures::database::TestDatabase,
    ) -> (MTeam, MTeam, ProjectId) {
        use gradient_fixtures::rows;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the user is written");
        crate::rows::insert::<ERole, _>(&conn, rows::write_role())
            .await
            .expect("the role is written");
        let owner = crate::rows::insert::<ETeam, _>(
            &conn,
            MTeam {
                new_project_users: true,
                new_project_workers: true,
                new_project_role: Some(BASE_ROLE_WRITE_ID),
                ..rows::team()
            },
        )
        .await
        .expect("the owner team is written");
        let other = crate::rows::insert::<ETeam, _>(
            &conn,
            MTeam {
                id: TeamId::now_v7(),
                name: "other".into(),
                new_project_workers: true,
                ..rows::team()
            },
        )
        .await
        .expect("the other team is written");
        let project = crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        (owner, other, project.id)
    }

    #[tokio::test]
    async fn only_the_owner_team_gets_a_grant_on_a_new_project() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let (owner, _other, project) = two_teams_and_a_project(&db).await;

        let change = upsert_project_owner_team_grant(&db.connection(), project, Some(owner.id))
            .await
            .expect("the owner grant is written");

        let grants = db.rows::<ETeamProject>().await;
        assert_eq!(change.added.as_ref(), grants.first());
        assert_eq!(
            grants
                .iter()
                .map(|g| (g.team, g.role, g.includes_users, g.grant_source))
                .collect::<Vec<_>>(),
            [(
                owner.id,
                Some(BASE_ROLE_WRITE_ID),
                true,
                TeamGrantSource::Owner
            )]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn every_team_with_the_workers_setting_gets_a_workers_only_grant_once() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let (owner, other, project) = two_teams_and_a_project(&db).await;
        upsert_project_owner_team_grant(&conn, project, Some(owner.id))
            .await
            .expect("the owner grant is written");

        let inserted =
            insert_worker_grants_for_teams_that_grant_workers_on_every_new_project(&conn, project)
                .await
                .expect("the worker grants are written");

        assert_eq!(
            inserted
                .iter()
                .map(|g| (g.team, g.role, g.includes_users, g.grant_source))
                .collect::<Vec<_>>(),
            [(
                other.id,
                None,
                false,
                TeamGrantSource::EveryProjectWorkerGrant
            )]
        );
        assert_eq!(db.rows::<ETeamProject>().await.len(), 2);
        db.finish().await;
    }

    #[tokio::test]
    async fn a_new_owner_team_replaces_the_old_owner_grant_but_not_manual_grants() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let (owner, other, project) = two_teams_and_a_project(&db).await;
        upsert_project_owner_team_grant(&conn, project, Some(owner.id))
            .await
            .expect("the owner grant is written");
        let manual = crate::rows::insert::<ETeamProject, _>(
            &conn,
            MTeamProject {
                id: TeamProjectId::now_v7(),
                team: other.id,
                project,
                includes_workers: true,
                created_at: gradient_types::now(),
                ..Default::default()
            },
        )
        .await
        .expect("the manual grant is written");

        let change = upsert_project_owner_team_grant(&conn, project, Some(other.id))
            .await
            .expect("the owner moves");

        assert_eq!(change.added, None);
        assert_eq!(
            change.removed.iter().map(|g| g.team).collect::<Vec<_>>(),
            [owner.id]
        );
        assert_eq!(db.rows::<ETeamProject>().await, [manual]);
        db.finish().await;
    }
}
