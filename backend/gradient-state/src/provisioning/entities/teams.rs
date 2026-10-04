/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::lookup_id;
use crate::config::*;
use anyhow::Result;
use gradient_entity::team_user::TeamRole;
use gradient_entity::*;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
    BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use std::collections::HashMap;

fn builtin_project_role(name: &str) -> Option<RoleId> {
    match name {
        "Admin" => Some(BASE_ROLE_ADMIN_ID),
        "Write" => Some(BASE_ROLE_WRITE_ID),
        "View" => Some(BASE_ROLE_VIEW_ID),
        _ => None,
    }
}

fn builtin_cache_role(name: &str) -> Option<RoleId> {
    match name {
        "Admin" => Some(BASE_CACHE_ROLE_ADMIN_ID),
        "Write" => Some(BASE_CACHE_ROLE_WRITE_ID),
        "View" => Some(BASE_CACHE_ROLE_VIEW_ID),
        _ => None,
    }
}

fn team_role(name: &str) -> TeamRole {
    match name {
        "Admin" => TeamRole::Admin,
        _ => TeamRole::Member,
    }
}

// The state only removes the grants it declared (`managed`), keeping API and new-project grants.
impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_teams(
        &self,
        state_teams: &HashMap<String, StateTeam>,
    ) -> Result<HashMap<String, TeamId>, DynError> {
        let user_map = self.user_lookup().await?;
        let mut team_ids = HashMap::new();

        for state_team in state_teams.values() {
            let team_id = self.upsert_team(state_team).await?;
            self.reconcile_team_members(team_id, state_team, &user_map)
                .await?;
            team_ids.insert(state_team.name.clone(), team_id);
        }

        Ok(team_ids)
    }

    async fn upsert_team(&self, state_team: &StateTeam) -> Result<TeamId, DynError> {
        let new_project_role = state_team
            .new_projects
            .role
            .as_deref()
            .and_then(builtin_project_role);
        let existing = team::Entity::find()
            .filter(team::Column::Name.eq(&state_team.name))
            .one(self.db)
            .await?;

        if let Some(row) = existing {
            let id = row.id;
            let mut active: team::ActiveModel = row.into();
            active.display_name = Set(state_team.display_name.clone());
            active.oidc_group = Set(state_team.oidc_group.clone());
            active.scim_group = Set(state_team.scim_group.clone());
            active.new_project_users = Set(state_team.new_projects.users);
            active.new_project_workers = Set(state_team.new_projects.workers);
            active.new_project_role = Set(new_project_role);
            active.managed = Set(true);
            active.update(self.db).await?;
            tracing::info!(team = %state_team.name, "Updated managed team");
            return Ok(id);
        }

        let id = TeamId::now_v7();
        team::Model {
            id,
            name: state_team.name.clone(),
            display_name: state_team.display_name.clone(),
            oidc_group: state_team.oidc_group.clone(),
            scim_group: state_team.scim_group.clone(),
            new_project_users: state_team.new_projects.users,
            new_project_workers: state_team.new_projects.workers,
            new_project_role,
            created_by: None,
            managed: true,
            created_at: now(),
        }
        .into_active_model()
        .insert(self.db)
        .await?;
        tracing::info!(team = %state_team.name, "Created managed team");
        Ok(id)
    }

    async fn reconcile_team_members(
        &self,
        team_id: TeamId,
        state_team: &StateTeam,
        user_map: &HashMap<String, UserId>,
    ) -> Result<(), DynError> {
        let mut declared = Vec::with_capacity(state_team.members.len());
        for member in &state_team.members {
            let user = lookup_id(user_map, &member.user, "User")?;
            let role = team_role(&member.role);
            declared.push(user);

            let existing = team_user::Entity::find()
                .filter(team_user::Column::Team.eq(team_id))
                .filter(team_user::Column::User.eq(user))
                .one(self.db)
                .await?;
            match existing {
                Some(row) if row.role == role && !row.via_group => {}
                Some(row) => {
                    let mut active: team_user::ActiveModel = row.into();
                    active.role = Set(role);
                    active.via_group = Set(false);
                    active.update(self.db).await?;
                }
                None => {
                    team_user::Model {
                        id: TeamUserId::now_v7(),
                        team: team_id,
                        user,
                        role,
                        via_group: false,
                    }
                    .into_active_model()
                    .insert(self.db)
                    .await?;
                }
            }
        }

        team_user::Entity::delete_many()
            .filter(team_user::Column::Team.eq(team_id))
            .filter(team_user::Column::ViaGroup.eq(false))
            .filter(team_user::Column::User.is_not_in(declared))
            .exec(self.db)
            .await?;

        Ok(())
    }

    pub(crate) async fn apply_project_teams(
        &self,
        state_projects: &HashMap<String, StateProject>,
        team_ids: &HashMap<String, TeamId>,
    ) -> Result<(), DynError> {
        let project_map = self.project_lookup().await?;

        for state_project in state_projects.values() {
            let project_id = lookup_id(&project_map, &state_project.name, "Project")?;
            let custom_roles: HashMap<String, RoleId> = role::Entity::find()
                .filter(role::Column::Project.eq(project_id))
                .filter(role::Column::Managed.eq(true))
                .all(self.db)
                .await?
                .into_iter()
                .map(|r| (r.name, r.id))
                .collect();

            let mut declared = Vec::with_capacity(state_project.teams.len());
            for grant in &state_project.teams {
                let team = lookup_id(team_ids, &grant.team, "Team")?;
                declared.push(team);
                let role = match (grant.users, grant.role.as_deref()) {
                    (false, _) | (true, None) => None,
                    (true, Some(name)) => Some(
                        builtin_project_role(name)
                            .or_else(|| custom_roles.get(name).copied())
                            .ok_or_else(|| -> DynError {
                                format!(
                                    "Project '{}' team '{}' references unknown role '{}'",
                                    state_project.name, grant.team, name
                                )
                                .into()
                            })?,
                    ),
                };
                self.upsert_project_grant(project_id, team, role, grant)
                    .await?;
            }

            remove_undeclared_project_grants(self.db, project_id, declared).await?;
        }

        Ok(())
    }

    async fn upsert_project_grant(
        &self,
        project: ProjectId,
        team: TeamId,
        role: Option<RoleId>,
        grant: &StateProjectTeam,
    ) -> Result<(), DynError> {
        let existing = team_project::Entity::find()
            .filter(team_project::Column::Project.eq(project))
            .filter(team_project::Column::Team.eq(team))
            .one(self.db)
            .await?;

        if let Some(row) = existing {
            let mut active: team_project::ActiveModel = row.into();
            active.role = Set(role);
            active.includes_users = Set(grant.users);
            active.includes_workers = Set(grant.workers);
            active.managed = Set(true);
            active.update(self.db).await?;
            return Ok(());
        }

        team_project::Model {
            id: TeamProjectId::now_v7(),
            team,
            project,
            role,
            includes_users: grant.users,
            includes_workers: grant.workers,
            managed: true,
            created_at: now(),
        }
        .into_active_model()
        .insert(self.db)
        .await?;
        Ok(())
    }

    pub(crate) async fn apply_cache_teams(
        &self,
        state_caches: &HashMap<String, StateCache>,
        team_ids: &HashMap<String, TeamId>,
    ) -> Result<(), DynError> {
        for state_cache in state_caches.values() {
            let cache_id = cache::Entity::find()
                .filter(cache::Column::Name.eq(&state_cache.name))
                .one(self.db)
                .await?
                .ok_or_else(|| format!("Cache '{}' not found", state_cache.name))?
                .id;

            let mut declared = Vec::with_capacity(state_cache.teams.len());
            for grant in &state_cache.teams {
                let team = lookup_id(team_ids, &grant.team, "Team")?;
                declared.push(team);
                let role = match builtin_cache_role(&grant.role) {
                    Some(id) => id,
                    None => {
                        cache_role::Entity::find()
                            .filter(cache_role::Column::Cache.eq(cache_id))
                            .filter(cache_role::Column::Name.eq(&grant.role))
                            .one(self.db)
                            .await?
                            .ok_or_else(|| {
                                format!(
                                    "Cache '{}' team '{}' references unknown role '{}'",
                                    state_cache.name, grant.team, grant.role
                                )
                            })?
                            .id
                    }
                };

                let existing = team_cache::Entity::find()
                    .filter(team_cache::Column::Cache.eq(cache_id))
                    .filter(team_cache::Column::Team.eq(team))
                    .one(self.db)
                    .await?;
                match existing {
                    Some(row) if row.role == role && row.managed => {}
                    Some(row) => {
                        let mut active: team_cache::ActiveModel = row.into();
                        active.role = Set(role);
                        active.managed = Set(true);
                        active.update(self.db).await?;
                    }
                    None => {
                        team_cache::Model {
                            id: TeamCacheId::now_v7(),
                            team,
                            cache: cache_id,
                            role,
                            managed: true,
                            created_at: now(),
                        }
                        .into_active_model()
                        .insert(self.db)
                        .await?;
                    }
                }
            }

            remove_undeclared_cache_grants(self.db, cache_id, declared).await?;
        }

        Ok(())
    }
}

async fn remove_undeclared_project_grants<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    declared: Vec<TeamId>,
) -> Result<(), sea_orm::DbErr> {
    team_project::Entity::delete_many()
        .filter(team_project::Column::Project.eq(project))
        .filter(team_project::Column::Managed.eq(true))
        .filter(team_project::Column::Team.is_not_in(declared))
        .exec(db)
        .await?;
    Ok(())
}

async fn remove_undeclared_cache_grants<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    declared: Vec<TeamId>,
) -> Result<(), sea_orm::DbErr> {
    team_cache::Entity::delete_many()
        .filter(team_cache::Column::Cache.eq(cache))
        .filter(team_cache::Column::Managed.eq(true))
        .filter(team_cache::Column::Team.is_not_in(declared))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod grant_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn deletes_only_managed_grants(db: sea_orm::DatabaseConnection, table: &str) {
        let delete = format!("DELETE FROM \"{table}\"");
        let statements: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .map(|s| s.sql)
            .filter(|sql| sql.starts_with(&delete))
            .collect();
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(statements[0].contains("\"managed\""), "{}", statements[0]);
    }

    fn executed() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
    }

    #[tokio::test]
    async fn an_empty_project_team_list_removes_the_grants_the_state_declared() {
        let db = executed().into_connection();
        remove_undeclared_project_grants(&db, ProjectId::now_v7(), Vec::new())
            .await
            .unwrap();
        deletes_only_managed_grants(db, "team_project");
    }

    #[tokio::test]
    async fn an_empty_cache_team_list_removes_the_grants_the_state_declared() {
        let db = executed().into_connection();
        remove_undeclared_cache_grants(&db, CacheId::now_v7(), Vec::new())
            .await
            .unwrap();
        deletes_only_managed_grants(db, "team_cache");
    }
}
