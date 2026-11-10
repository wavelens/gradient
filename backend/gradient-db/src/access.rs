/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
    BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
};
use gradient_types::permissions::PermissionMask;
use gradient_types::*;
use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::HashMap;

pub async fn project_permission_mask<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    user: UserId,
) -> Result<Option<PermissionMask>, DbErr> {
    let roles = project_roles(db, project, user).await?;
    if roles.is_empty() {
        return Ok(None);
    }

    let mask = ERole::find()
        .filter(CRole::Id.is_in(roles))
        .all(db)
        .await?
        .into_iter()
        .fold(0, |mask, role| mask | role.permission);
    Ok(Some(mask))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheRoleAndPermissionMask {
    pub role: Option<String>,
    pub permissions: PermissionMask,
}

pub async fn cache_permission_mask<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    user: UserId,
) -> Result<Option<PermissionMask>, DbErr> {
    Ok(load_cache_member_role_and_permission_mask(db, cache, user)
        .await?
        .map(|grant| grant.permissions))
}

pub async fn load_cache_member_role_and_permission_mask<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    user: UserId,
) -> Result<Option<CacheRoleAndPermissionMask>, DbErr> {
    let roles: Vec<RoleId> = ECacheAccess::find()
        .filter(CCacheAccess::Cache.eq(cache))
        .filter(CCacheAccess::User.eq(user))
        .all(db)
        .await?
        .into_iter()
        .map(|access| access.role)
        .collect();
    if roles.is_empty() {
        return Ok(None);
    }

    let roles = ECacheRole::find()
        .filter(CCacheRole::Id.is_in(roles))
        .all(db)
        .await?;
    let permissions = roles.iter().fold(0, |mask, role| mask | role.permission);
    let role = roles
        .into_iter()
        .max_by_key(|role| cache_role_rank(role.id))
        .map(|role| role.name);
    Ok(Some(CacheRoleAndPermissionMask { role, permissions }))
}

pub fn cache_role_rank(role: RoleId) -> u8 {
    match role {
        BASE_CACHE_ROLE_ADMIN_ID => 3,
        BASE_CACHE_ROLE_WRITE_ID => 2,
        BASE_CACHE_ROLE_VIEW_ID => 0,
        _ => 1,
    }
}

pub async fn reaches_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    user: UserId,
) -> Result<bool, DbErr> {
    Ok(!project_roles(db, project, user).await?.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectGrant {
    pub role: String,
    pub permissions: PermissionMask,
}

pub async fn project_grants<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    projects: Vec<ProjectId>,
) -> Result<HashMap<ProjectId, ProjectGrant>, DbErr> {
    let access = EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user))
        .filter(CProjectAccess::Project.is_in(projects))
        .all(db)
        .await?;
    if access.is_empty() {
        return Ok(HashMap::new());
    }
    let roles: HashMap<RoleId, MRole> = ERole::find()
        .filter(CRole::Id.is_in(access.iter().map(|a| a.role)))
        .all(db)
        .await?
        .into_iter()
        .map(|role| (role.id, role))
        .collect();

    let mut strongest: HashMap<ProjectId, RoleId> = HashMap::new();
    let mut masks: HashMap<ProjectId, PermissionMask> = HashMap::new();
    for row in access {
        if let Some(role) = roles.get(&row.role) {
            *masks.entry(row.project).or_default() |= role.permission;
        }
        strongest
            .entry(row.project)
            .and_modify(|role| {
                if role_rank(row.role) > role_rank(*role) {
                    *role = row.role;
                }
            })
            .or_insert(row.role);
    }
    Ok(strongest
        .into_iter()
        .filter_map(|(project, role)| {
            let grant = ProjectGrant {
                role: roles.get(&role)?.name.clone(),
                permissions: masks.get(&project).copied().unwrap_or_default(),
            };
            Some((project, grant))
        })
        .collect())
}

pub async fn project_role_names<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    projects: Vec<ProjectId>,
) -> Result<HashMap<ProjectId, String>, DbErr> {
    Ok(project_grants(db, user, projects)
        .await?
        .into_iter()
        .map(|(project, grant)| (project, grant.role))
        .collect())
}

pub fn role_rank(role: RoleId) -> u8 {
    match role {
        BASE_ROLE_ADMIN_ID => 3,
        BASE_ROLE_WRITE_ID => 2,
        BASE_ROLE_VIEW_ID => 0,
        _ => 1,
    }
}

async fn project_roles<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    user: UserId,
) -> Result<Vec<RoleId>, DbErr> {
    Ok(EProjectAccess::find()
        .filter(CProjectAccess::Project.eq(project))
        .filter(CProjectAccess::User.eq(user))
        .all(db)
        .await?
        .into_iter()
        .map(|access| access.role)
        .collect())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct RevokedUserAccessCounts {
    pub projects: u64,
    pub caches: u64,
    pub teams: u64,
    pub invitations: u64,
}

pub async fn delete_user_memberships_and_pending_invitations_skipping_configured_rows<
    C: ConnectionTrait,
>(
    db: &C,
    user: UserId,
) -> Result<RevokedUserAccessCounts, DbErr> {
    let projects = EProjectUser::delete_many()
        .filter(CProjectUser::User.eq(user))
        .filter(
            CProjectUser::Project.not_in_subquery(
                Query::select()
                    .column(CProject::Id)
                    .from(EProject::default())
                    .and_where(CProject::Managed.eq(true))
                    .to_owned(),
            ),
        )
        .exec(db)
        .await?
        .rows_affected;
    let caches = ECacheUser::delete_many()
        .filter(CCacheUser::User.eq(user))
        .filter(
            CCacheUser::Cache.not_in_subquery(
                Query::select()
                    .column(CCache::Id)
                    .from(ECache::default())
                    .and_where(CCache::Managed.eq(true))
                    .to_owned(),
            ),
        )
        .exec(db)
        .await?
        .rows_affected;
    let teams = ETeamUser::delete_many()
        .filter(CTeamUser::User.eq(user))
        .filter(CTeamUser::Source.eq(TeamMemberSource::Api))
        .exec(db)
        .await?
        .rows_affected;
    let invitations = EProjectInvitation::delete_many()
        .filter(CProjectInvitation::User.eq(user))
        .exec(db)
        .await?
        .rows_affected
        + ECacheInvitation::delete_many()
            .filter(CCacheInvitation::User.eq(user))
            .exec(db)
            .await?
            .rows_affected
        + ETeamInvitation::delete_many()
            .filter(CTeamInvitation::User.eq(user))
            .exec(db)
            .await?
            .rows_affected;
    Ok(RevokedUserAccessCounts {
        projects,
        caches,
        teams,
        invitations,
    })
}

#[derive(Debug, Default)]
pub struct DirectMembershipsOfUser {
    pub projects: Vec<MProjectUser>,
    pub caches: Vec<MCacheUser>,
}

pub async fn direct_memberships_of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<DirectMembershipsOfUser, DbErr> {
    Ok(DirectMembershipsOfUser {
        projects: EProjectUser::find()
            .filter(CProjectUser::User.eq(user))
            .all(db)
            .await?,
        caches: ECacheUser::find()
            .filter(CCacheUser::User.eq(user))
            .all(db)
            .await?,
    })
}

pub async fn superusers<C: ConnectionTrait>(db: &C) -> Result<Vec<MUser>, DbErr> {
    EUser::find().filter(CUser::Superuser.eq(true)).all(db).await
}

async fn team_members<C: ConnectionTrait>(
    db: &C,
    teams: impl IntoIterator<Item = TeamId>,
) -> Result<Vec<MTeamUser>, DbErr> {
    ETeamUser::find()
        .filter(CTeamUser::Team.is_in(teams))
        .all(db)
        .await
}

async fn teams_of_user<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Vec<TeamId>, DbErr> {
    Ok(ETeamUser::find()
        .filter(CTeamUser::User.eq(user))
        .all(db)
        .await?
        .into_iter()
        .map(|membership| membership.team)
        .collect())
}

fn pair_with_members<G>(
    grants: Vec<G>,
    members: &[MTeamUser],
    team: impl Fn(&G) -> TeamId,
) -> Vec<(G, UserId)>
where
    G: Clone,
{
    grants
        .into_iter()
        .flat_map(|grant| {
            members
                .iter()
                .filter(|member| member.team == team(&grant))
                .map(|member| (grant.clone(), member.user))
                .collect::<Vec<_>>()
        })
        .collect()
}

pub async fn list_team_project_grants_with_users<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<(MTeamProject, UserId)>, DbErr> {
    let grants = ETeamProject::find()
        .filter(CTeamProject::Project.eq(project))
        .filter(CTeamProject::IncludesUsers.eq(true))
        .filter(CTeamProject::Role.is_not_null())
        .all(db)
        .await?;
    let members = team_members(db, grants.iter().map(|grant| grant.team)).await?;
    Ok(pair_with_members(grants, &members, |grant| grant.team))
}

pub async fn list_team_cache_grants_with_users<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<(MTeamCache, UserId)>, DbErr> {
    let grants = ETeamCache::find()
        .filter(CTeamCache::Cache.eq(cache))
        .all(db)
        .await?;
    let members = team_members(db, grants.iter().map(|grant| grant.team)).await?;
    Ok(pair_with_members(grants, &members, |grant| grant.team))
}

pub async fn team_project_grants_reaching_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<MTeamProject>, DbErr> {
    ETeamProject::find()
        .filter(CTeamProject::Team.is_in(teams_of_user(db, user).await?))
        .filter(CTeamProject::IncludesUsers.eq(true))
        .filter(CTeamProject::Role.is_not_null())
        .all(db)
        .await
}

pub async fn team_cache_grants_reaching_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<MTeamCache>, DbErr> {
    ETeamCache::find()
        .filter(CTeamCache::Team.is_in(teams_of_user(db, user).await?))
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use gradient_types::permissions::{admin_mask, cache_write_mask, view_mask};

    fn access(role: RoleId) -> MProjectAccess {
        MProjectAccess {
            project: ProjectId::now_v7(),
            user: UserId::now_v7(),
            role,
            team: None,
        }
    }

    #[tokio::test]
    async fn a_user_without_any_access_has_no_mask() {
        let db = mock_db()
            .append_query_results([Vec::<MProjectAccess>::new()])
            .into_connection();

        let mask = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query");
        assert!(mask.is_none());
    }

    #[tokio::test]
    async fn the_strongest_role_names_the_access_to_a_project() {
        use gradient_types::consts::{BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
        let (project, user) = (ProjectId::now_v7(), UserId::now_v7());
        let row = |role| MProjectAccess {
            project,
            user,
            role,
            team: None,
        };
        let named = |id, name: &str| MRole {
            id,
            name: name.into(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![row(BASE_ROLE_VIEW_ID), row(BASE_ROLE_WRITE_ID)]])
            .append_query_results([vec![
                named(BASE_ROLE_VIEW_ID, "View"),
                named(BASE_ROLE_WRITE_ID, "Write"),
            ]])
            .into_connection();

        let names = project_role_names(&db, user, vec![project])
            .await
            .expect("query");
        assert_eq!(names.get(&project).map(String::as_str), Some("Write"));
    }

    #[tokio::test]
    async fn a_team_grant_alone_reaches_the_project() {
        let db = mock_db()
            .append_query_results([vec![access(RoleId::now_v7())]])
            .into_connection();

        assert!(
            reaches_project(&db, ProjectId::now_v7(), UserId::now_v7())
                .await
                .expect("query")
        );
    }

    #[tokio::test]
    async fn a_missing_role_row_grants_nothing() {
        let db = mock_db()
            .append_query_results([vec![access(RoleId::now_v7())]])
            .append_query_results([Vec::<MRole>::new()])
            .into_connection();

        let mask = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query");
        assert_eq!(mask, Some(0));
    }

    #[tokio::test]
    async fn the_strongest_cache_role_names_the_membership() {
        let row = |role| MCacheAccess {
            cache: CacheId::now_v7(),
            user: UserId::now_v7(),
            role,
            team: None,
        };
        let role = |id, name: &str| MCacheRole {
            id,
            name: name.into(),
            ..Default::default()
        };
        let custom = RoleId::now_v7();
        let db = mock_db()
            .append_query_results([vec![row(BASE_CACHE_ROLE_VIEW_ID), row(custom)]])
            .append_query_results([vec![
                role(BASE_CACHE_ROLE_VIEW_ID, "View"),
                role(custom, "Uploader"),
            ]])
            .into_connection();

        let grant =
            load_cache_member_role_and_permission_mask(&db, CacheId::now_v7(), UserId::now_v7())
                .await
                .expect("query")
                .expect("access");
        assert_eq!(grant.role.as_deref(), Some("Uploader"));
    }

    async fn team_member_with_project(conn: &sea_orm::DatabaseConnection) {
        use gradient_fixtures::rows;
        crate::rows::insert::<EUser, _>(conn, rows::user())
            .await
            .expect("the user is written");
        crate::rows::insert::<ETeam, _>(conn, rows::team())
            .await
            .expect("the team is written");
        crate::rows::insert::<ETeamUser, _>(conn, rows::team_member(TeamRole::Member))
            .await
            .expect("the membership is written");
        for role in [rows::admin_role(), rows::write_role(), rows::view_role()] {
            crate::rows::insert::<ERole, _>(conn, role)
                .await
                .expect("the role is written");
        }
        crate::rows::insert::<EProject, _>(conn, rows::project())
            .await
            .expect("the project is written");
        crate::rows::insert::<ECache, _>(conn, rows::cache())
            .await
            .expect("the cache is written");
    }

    fn team_grant(role: RoleId) -> MTeamProject {
        MTeamProject {
            id: TeamProjectId::now_v7(),
            team: gradient_fixtures::ids::team_id(),
            project: gradient_fixtures::ids::project_id(),
            role: Some(role),
            includes_users: true,
            created_at: gradient_types::now(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_direct_role_overrides_a_stronger_team_grant() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        team_member_with_project(&conn).await;
        crate::rows::insert::<ETeamProject, _>(&conn, team_grant(BASE_ROLE_ADMIN_ID))
            .await
            .expect("the grant is written");
        let direct = crate::rows::insert::<EProjectUser, _>(
            &conn,
            gradient_fixtures::rows::project_member(BASE_ROLE_VIEW_ID),
        )
        .await
        .expect("the member is written");
        let (project, user) = (direct.project, direct.user);

        let with_direct_row = project_permission_mask(&conn, project, user)
            .await
            .expect("the mask reads");
        crate::rows::delete::<EProjectUser, _>(&conn, direct)
            .await
            .expect("the member is removed");
        let with_team_grant_only = project_permission_mask(&conn, project, user)
            .await
            .expect("the mask reads");

        assert_eq!(with_direct_row, Some(view_mask()));
        assert_eq!(with_team_grant_only, Some(admin_mask()));
        db.finish().await;
    }

    #[tokio::test]
    async fn a_direct_cache_role_overrides_a_stronger_team_grant() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        team_member_with_project(&conn).await;
        crate::rows::insert::<ETeamCache, _>(
            &conn,
            MTeamCache {
                id: TeamCacheId::now_v7(),
                team: gradient_fixtures::ids::team_id(),
                cache: gradient_fixtures::ids::cache_id(),
                role: BASE_CACHE_ROLE_WRITE_ID,
                created_at: gradient_types::now(),
                ..Default::default()
            },
        )
        .await
        .expect("the grant is written");
        crate::rows::insert::<ECacheUser, _>(
            &conn,
            gradient_fixtures::rows::cache_member(BASE_CACHE_ROLE_VIEW_ID),
        )
        .await
        .expect("the member is written");

        let grant = load_cache_member_role_and_permission_mask(
            &conn,
            gradient_fixtures::ids::cache_id(),
            gradient_fixtures::ids::user_id(),
        )
        .await
        .expect("the mask reads")
        .expect("access");

        assert_eq!(grant.role.as_deref(), Some("View"));
        assert!(grant.permissions & cache_write_mask() != cache_write_mask());
        db.finish().await;
    }

    #[tokio::test]
    async fn revoking_all_access_keeps_rows_of_the_server_configuration() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        team_member_with_project(&conn).await;
        crate::rows::insert::<EProjectUser, _>(
            &conn,
            gradient_fixtures::rows::project_member(BASE_ROLE_WRITE_ID),
        )
        .await
        .expect("the member is written");
        let configured = crate::rows::insert::<EProject, _>(
            &conn,
            MProject {
                managed: true,
                ..gradient_fixtures::rows::project_named("configured")
            },
        )
        .await
        .expect("the configured project is written");
        crate::rows::insert::<EProjectUser, _>(
            &conn,
            MProjectUser {
                project: configured.id,
                ..gradient_fixtures::rows::project_member(BASE_ROLE_VIEW_ID)
            },
        )
        .await
        .expect("the configured member is written");

        let revoked = delete_user_memberships_and_pending_invitations_skipping_configured_rows(
            &conn,
            gradient_fixtures::ids::user_id(),
        )
        .await
        .expect("the rows are deleted");

        assert_eq!(
            revoked,
            RevokedUserAccessCounts {
                projects: 1,
                teams: 1,
                ..Default::default()
            }
        );
        let kept: Vec<ProjectId> = db
            .rows::<EProjectUser>()
            .await
            .into_iter()
            .map(|row| row.project)
            .collect();
        assert_eq!(kept, [configured.id]);
        db.finish().await;
    }

    #[tokio::test]
    async fn the_precedence_migration_deletes_only_direct_rows_weaker_than_team_grants() {
        use gradient_migration::{Migrator, MigratorTrait};
        use sea_orm_migration::SchemaManager;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name() == "m20261203_000000_project_owner_team_and_direct_role_precedence")
            .expect("the migration is registered");
        migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("the migration reverts");
        team_member_with_project_before_precedence(&db).await;
        let (project, team) = (
            gradient_fixtures::ids::project_id(),
            gradient_fixtures::ids::team_id(),
        );
        let (weaker, stronger) = (UserId::now_v7(), UserId::now_v7());
        for (user, role) in [(weaker, BASE_ROLE_VIEW_ID), (stronger, BASE_ROLE_ADMIN_ID)] {
            db.execute(&format!(
                "INSERT INTO \"user\" (id, username, name, email, last_login_at, created_at) \
                 VALUES ('{user}', 'u{}', 'U', 'u{}@example.com', now(), now()); \
                 INSERT INTO team_user (id, team, \"user\", role) VALUES ('{}', '{team}', '{user}', 1); \
                 INSERT INTO project_user (id, project, \"user\", role) VALUES ('{}', '{project}', '{user}', '{role}');",
                user.into_inner().simple(),
                user.into_inner().simple(),
                uuid::Uuid::now_v7(),
                uuid::Uuid::now_v7(),
            ))
            .await;
        }
        db.execute(&format!(
            "INSERT INTO team_project (id, team, project, role, includes_users, includes_workers, managed, created_at) \
             VALUES ('{}', '{team}', '{project}', '{BASE_ROLE_WRITE_ID}', true, false, true, now());",
            uuid::Uuid::now_v7(),
        ))
        .await;

        migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("the migration applies");

        let direct: Vec<UserId> = db
            .rows::<EProjectUser>()
            .await
            .into_iter()
            .map(|row| row.user)
            .collect();
        assert_eq!(direct, [stronger]);
        assert_eq!(
            project_permission_mask(&conn, project, weaker)
                .await
                .expect("the mask reads"),
            Some(gradient_types::permissions::write_mask())
        );
        assert_eq!(
            db.rows::<ETeamProject>().await[0].grant_source,
            TeamGrantSource::State
        );
        db.finish().await;
    }

    async fn team_member_with_project_before_precedence(
        db: &gradient_fixtures::database::TestDatabase,
    ) {
        use gradient_fixtures::rows;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the user is written");
        crate::rows::insert::<ETeam, _>(&conn, rows::team())
            .await
            .expect("the team is written");
        for role in [rows::admin_role(), rows::write_role(), rows::view_role()] {
            crate::rows::insert::<ERole, _>(&conn, role)
                .await
                .expect("the role is written");
        }
        db.execute(&format!(
            "INSERT INTO project (id, name, display_name, description, public_key, private_key, \
             public, hide_build_requests, allow_exec_in_build_requests, created_by, created_at) \
             VALUES ('{}', 'p', 'P', '', '', '', false, false, false, '{}', now());",
            gradient_fixtures::ids::project_id(),
            gradient_fixtures::ids::user_id(),
        ))
        .await;
    }
}
