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
use sea_orm::{ConnectionTrait, EntityTrait};
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

impl<'a> StateApplicator<'a> {
    pub(crate) async fn user_lookup(&self) -> Result<HashMap<String, UserId>, DynError> {
        let users = user::Entity::find().all(self.db).await?;
        Ok(users.into_iter().map(|u| (u.username, u.id)).collect())
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
