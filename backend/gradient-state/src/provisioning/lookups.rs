/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DynError;
use super::StateApplicator;
use gradient_db::git_host_connections::{self, GitHostConnectionOwnerByDatabaseId};
use gradient_db::lookup::by_id;
use gradient_entity::*;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;

pub(crate) fn lookup_id<T: Copy>(
    map: &HashMap<String, T>,
    name: &str,
    kind: &str,
) -> Result<T, DynError> {
    map.get(name)
        .copied()
        .ok_or_else(|| format!("{} '{}' not found", kind, name).into())
}

pub(crate) async fn load_git_host_connections_usable_by_project_by_name<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<HashMap<String, MGitHostConnection>, DynError> {
    let project = by_id::<EProject, _>(db, project_id)
        .await?
        .ok_or_else(|| format!("Project '{}' not found", project_id))?;
    let usable =
        git_host_connections::list_git_host_connections_available_to_project(db, &project).await?;
    Ok(keep_closest_owner_git_host_connection_for_each_name(usable))
}

pub(crate) fn keep_closest_owner_git_host_connection_for_each_name(
    connections: Vec<MGitHostConnection>,
) -> HashMap<String, MGitHostConnection> {
    let rank = |connection: &MGitHostConnection| match GitHostConnectionOwnerByDatabaseId::of(
        connection,
    ) {
        GitHostConnectionOwnerByDatabaseId::Project(_) => 0,
        GitHostConnectionOwnerByDatabaseId::Team(_) => 1,
        GitHostConnectionOwnerByDatabaseId::Instance => 2,
    };
    let mut closest: HashMap<String, MGitHostConnection> = HashMap::new();
    for connection in connections {
        match closest.get(&connection.name) {
            Some(kept) if rank(kept) <= rank(&connection) => {}
            _ => {
                closest.insert(connection.name.clone(), connection);
            }
        }
    }
    closest
}

pub(crate) struct StateFileManagedUserIdsByUsername(HashMap<String, UserId>);

impl StateFileManagedUserIdsByUsername {
    pub(crate) fn resolve_username_to_user_id(
        &self,
        username: &str,
        grant: &str,
    ) -> Option<UserId> {
        let user_id = self.0.get(&username.to_lowercase()).copied();
        if user_id.is_none() {
            tracing::error!(
                username,
                grant,
                "Skip grant to state user: records in the state file declare this user, but none of the accounts in the state file have that username. An account from another origin already took that username. See the earlier conflict error."
            );
        }
        user_id
    }

    pub(crate) fn resolve_creator_username_to_state_account_id(
        &self,
        username: &str,
        owner: &str,
        existing_entity_created_by_user_id: Option<UserId>,
    ) -> Result<UserId, DynError> {
        if let Some(user_id) = self.0.get(&username.to_lowercase()).copied() {
            return Ok(user_id);
        }
        if username.is_empty() {
            return existing_entity_created_by_user_id.ok_or_else(|| {
                format!("No creator for {owner} (created_by is empty). {owner} doesn't exist yet. Set created_by to the user named in the state file.").into()
            });
        }
        match existing_entity_created_by_user_id {
            Some(existing_entity_created_by_user_id) => {
                tracing::warn!(
                    username,
                    owner,
                    %existing_entity_created_by_user_id,
                    "Keep the creator stored on the existing entity. An account from another origin can have the same username as the declared creator. The state file can't use that account. See the earlier conflict error."
                );
                Ok(existing_entity_created_by_user_id)
            }
            None => Err(format!(
                "User '{username}' is the creator of {owner}, but {owner} does not exist yet. There is an account on another origin with this username. The state file cannot use this username. Rename or delete that account. Or set existing_account_id on the declared user to link it to that account. Such links remove the password, sessions, API keys and external ID links of that account."
            )
            .into()),
        }
    }
}

impl<'a> StateApplicator<'a> {
    pub(crate) async fn load_managed_accounts_by_lowercase_user_name(
        &self,
    ) -> Result<StateFileManagedUserIdsByUsername, DynError> {
        let users = user::Entity::find()
            .filter(user::Column::AccountOrigin.eq(AccountOrigin::DeclaredInStateFile))
            .filter(user::Column::Managed.eq(true))
            .all(self.db)
            .await?;
        Ok(StateFileManagedUserIdsByUsername(
            users
                .into_iter()
                .map(|u| (u.username.to_lowercase(), u.id))
                .collect(),
        ))
    }

    pub(crate) async fn project_lookup(&self) -> Result<HashMap<String, ProjectId>, DynError> {
        let projects = project::Entity::find().all(self.db).await?;
        Ok(projects.into_iter().map(|o| (o.name, o.id)).collect())
    }
}

#[cfg(test)]
mod closest_owner_tests {
    use super::*;

    fn connection(
        name: &str,
        project: Option<ProjectId>,
        owner_team: Option<TeamId>,
    ) -> MGitHostConnection {
        MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            name: name.into(),
            project,
            owner_team,
            ..Default::default()
        }
    }

    #[test]
    fn project_connection_wins_over_team_and_team_over_instance() {
        let (project, team) = (ProjectId::now_v7(), TeamId::now_v7());
        let instance_forge = connection("forge", None, None);
        let team_forge = connection("forge", None, Some(team));
        let project_forge = connection("forge", Some(project), None);
        let team_only = connection("team-only", None, Some(team));
        let instance_only = connection("instance-only", None, None);
        let instance_team_only = connection("team-only", None, None);

        let closest = keep_closest_owner_git_host_connection_for_each_name(vec![
            instance_forge,
            team_forge,
            project_forge.clone(),
            instance_team_only,
            team_only.clone(),
            instance_only.clone(),
        ]);

        assert_eq!(closest.len(), 3);
        assert_eq!(closest["forge"].id, project_forge.id);
        assert_eq!(closest["team-only"].id, team_only.id);
        assert_eq!(closest["instance-only"].id, instance_only.id);
    }
}
