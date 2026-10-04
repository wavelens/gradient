/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::PermissionMask;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
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

pub async fn cache_permission_mask<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    user: UserId,
) -> Result<Option<PermissionMask>, DbErr> {
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

    let mask = ECacheRole::find()
        .filter(CCacheRole::Id.is_in(roles))
        .all(db)
        .await?
        .into_iter()
        .fold(0, |mask, role| mask | role.permission);
    Ok(Some(mask))
}

pub async fn reaches_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    user: UserId,
) -> Result<bool, DbErr> {
    Ok(!project_roles(db, project, user).await?.is_empty())
}

pub async fn project_role_names<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    projects: Vec<ProjectId>,
) -> Result<HashMap<ProjectId, String>, DbErr> {
    let access = EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user))
        .filter(CProjectAccess::Project.is_in(projects))
        .all(db)
        .await?;
    if access.is_empty() {
        return Ok(HashMap::new());
    }
    let names: HashMap<RoleId, String> = ERole::find()
        .filter(CRole::Id.is_in(access.iter().map(|a| a.role)))
        .all(db)
        .await?
        .into_iter()
        .map(|role| (role.id, role.name))
        .collect();

    let mut strongest: HashMap<ProjectId, RoleId> = HashMap::new();
    for row in access {
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
        .filter_map(|(project, role)| names.get(&role).map(|name| (project, name.clone())))
        .collect())
}

fn role_rank(role: RoleId) -> u8 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{
        CachePermission, Permission, cache_mask_from, cache_mask_grants, mask_from, mask_grants,
    };
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn access(role: RoleId) -> MProjectAccess {
        MProjectAccess {
            project: ProjectId::now_v7(),
            user: UserId::now_v7(),
            role,
        }
    }

    #[tokio::test]
    async fn a_user_without_any_access_has_no_mask() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MProjectAccess>::new()])
            .into_connection();

        let mask = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query");
        assert!(mask.is_none());
    }

    #[tokio::test]
    async fn a_direct_role_and_a_team_role_add_up() {
        let (direct, team) = (RoleId::now_v7(), RoleId::now_v7());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![access(direct), access(team)]])
            .append_query_results([vec![
                MRole {
                    id: direct,
                    permission: mask_from(&[Permission::ViewProject]),
                    ..Default::default()
                },
                MRole {
                    id: team,
                    permission: mask_from(&[Permission::TriggerEvaluation]),
                    ..Default::default()
                },
            ]])
            .into_connection();

        let mask = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query")
            .expect("access");
        assert!(mask_grants(mask, Permission::ViewProject));
        assert!(mask_grants(mask, Permission::TriggerEvaluation));
        assert!(!mask_grants(mask, Permission::ManageMembers));
    }

    #[tokio::test]
    async fn the_strongest_role_names_the_access_to_a_project() {
        use gradient_types::consts::{BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
        let (project, user) = (ProjectId::now_v7(), UserId::now_v7());
        let row = |role| MProjectAccess {
            project,
            user,
            role,
        };
        let named = |id, name: &str| MRole {
            id,
            name: name.into(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![access(RoleId::now_v7())]])
            .append_query_results([Vec::<MRole>::new()])
            .into_connection();

        let mask = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query");
        assert_eq!(mask, Some(0));
    }

    #[tokio::test]
    async fn cache_roles_from_a_team_and_a_membership_add_up() {
        let (direct, team) = (RoleId::now_v7(), RoleId::now_v7());
        let row = |role| MCacheAccess {
            cache: CacheId::now_v7(),
            user: UserId::now_v7(),
            role,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(direct), row(team)]])
            .append_query_results([vec![
                MCacheRole {
                    id: direct,
                    permission: cache_mask_from(&[CachePermission::ReadStore]),
                    ..Default::default()
                },
                MCacheRole {
                    id: team,
                    permission: cache_mask_from(&[CachePermission::WriteStore]),
                    ..Default::default()
                },
            ]])
            .into_connection();

        let mask = cache_permission_mask(&db, CacheId::now_v7(), UserId::now_v7())
            .await
            .expect("query")
            .expect("access");
        assert!(cache_mask_grants(mask, CachePermission::ReadStore));
        assert!(cache_mask_grants(mask, CachePermission::WriteStore));
    }
}
