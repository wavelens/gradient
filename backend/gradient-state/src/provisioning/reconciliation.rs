/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DynError;
use super::StateApplicator;
use crate::config::StateConfiguration;
use gradient_db::git_host_connections::GitHostConnectionOwnerByDatabaseId;
use gradient_entity::*;
use gradient_types::git_host::GitHostConnectionOwner;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

/// Each set is built from the value's `name` or `username` field, the same field every `apply_*`
/// writes. The attrset key would unmanage or delete rows whose Nix `name` was overridden away from
/// it.
pub(crate) struct ManagedKeepSets<'a> {
    usernames: HashSet<&'a String>,
    project_names: HashSet<&'a String>,
    task_names: HashSet<&'a String>,
    cache_names: HashSet<&'a String>,
    api_key_names: HashSet<&'a String>,
    team_names: HashSet<&'a String>,
}

pub(crate) fn managed_keep_sets(config: &StateConfiguration) -> ManagedKeepSets<'_> {
    ManagedKeepSets {
        usernames: config.users.values().map(|u| &u.username).collect(),
        project_names: config.projects.values().map(|o| &o.name).collect(),
        task_names: config.tasks.values().map(|p| &p.name).collect(),
        cache_names: config.caches.values().map(|c| &c.name).collect(),
        api_key_names: config.api_keys.values().map(|k| &k.name).collect(),
        team_names: config.teams.values().map(|t| &t.name).collect(),
    }
}

macro_rules! unmark_managed {
    ($db:expr, $entity:ident, $state_set:expr, $name_field:ident, $delete_state:expr, $label:literal) => {{
        let managed = $entity::Entity::find()
            .filter($entity::Column::Managed.eq(true))
            .all($db)
            .await?;
        for model in managed {
            if $state_set.contains(&model.$name_field) {
                continue;
            }
            let label_value = model.$name_field.clone();
            if $delete_state {
                $entity::Entity::delete_by_id(model.id).exec($db).await?;
                tracing::info!(kind = $label, name = %label_value, "Deleted managed entity");
            } else {
                let mut active: $entity::ActiveModel = model.into();
                active.managed = Set(false);
                active.update($db).await?;
                tracing::info!(kind = $label, name = %label_value, "Unmanaged entity");
            }
        }
    }};
}

impl<'a> StateApplicator<'a> {
    pub(crate) async fn unmark_removed_entities(
        &self,
        config: &StateConfiguration,
        delete_state: bool,
    ) -> Result<(), DynError> {
        let ManagedKeepSets {
            usernames,
            project_names,
            task_names,
            cache_names,
            api_key_names,
            team_names,
        } = managed_keep_sets(config);
        let project_lookup = self.project_lookup().await?;
        let worker_keys: HashSet<(String, ProjectId)> = config
            .workers
            .values()
            .flat_map(|worker| {
                worker.projects.iter().filter_map(|project| {
                    project_lookup
                        .get(project)
                        .map(|peer_id| (worker.worker_id.clone(), *peer_id))
                })
            })
            .collect();
        let project_name_by_id: HashMap<ProjectId, String> = project_lookup
            .into_iter()
            .map(|(name, id)| (id, name))
            .collect();

        let db = self.db;

        unmark_managed!(db, user, usernames, username, delete_state, "user");
        unmark_managed!(db, project, project_names, name, delete_state, "project");
        unmark_managed!(db, task, task_names, name, delete_state, "task");
        unmark_managed!(db, cache, cache_names, name, delete_state, "cache");
        unmark_managed!(db, api, api_key_names, name, delete_state, "API key");
        unmark_managed!(db, team, team_names, name, delete_state, "team");

        self.unmark_removed_roles(config, &project_name_by_id, delete_state)
            .await?;
        let team_name_by_id: HashMap<TeamId, String> = team::Entity::find()
            .all(db)
            .await?
            .into_iter()
            .map(|team| (team.id, team.name))
            .collect();
        self.delete_or_unmark_managed_git_host_connections_not_declared_in_state(
            config,
            &project_name_by_id,
            &team_name_by_id,
            delete_state,
        )
        .await?;

        let managed_workers = worker_registration::Entity::find()
            .filter(worker_registration::Column::Managed.eq(true))
            .all(db)
            .await?;
        for reg in managed_workers {
            let key = (reg.worker_id.clone(), reg.peer_id);
            if !worker_keys.contains(&key) {
                let worker_id = reg.worker_id.clone();
                let peer_id = reg.peer_id;
                worker_registration::Entity::delete_by_id(reg.id)
                    .exec(db)
                    .await?;
                tracing::info!(
                    worker_id,
                    %peer_id,
                    "Deleted worker registration"
                );
            }
        }

        let team_worker_ids: HashSet<&String> = config
            .workers
            .values()
            .filter(|w| w.team.is_some())
            .map(|w| &w.worker_id)
            .collect();
        let managed_team_workers = team_worker::Entity::find()
            .filter(team_worker::Column::Managed.eq(true))
            .all(db)
            .await?;
        for worker in managed_team_workers {
            if team_worker_ids.contains(&worker.worker_id) {
                continue;
            }
            let worker_id = worker.worker_id.clone();
            team_worker::Entity::delete_by_id(worker.id)
                .exec(db)
                .await?;
            tracing::info!(worker_id, "Deleted team worker");
        }

        Ok(())
    }

    async fn unmark_removed_roles(
        &self,
        config: &StateConfiguration,
        project_name_by_id: &HashMap<ProjectId, String>,
        delete_state: bool,
    ) -> Result<(), DynError> {
        let role_keys: HashSet<(&str, &str)> = config
            .roles
            .values()
            .map(|r| (r.project.as_str(), r.name.as_str()))
            .collect();
        let managed_roles = role::Entity::find()
            .filter(role::Column::Managed.eq(true))
            .all(self.db)
            .await?;
        for managed in managed_roles {
            let Some(owner_name) = managed.project.and_then(|id| project_name_by_id.get(&id))
            else {
                continue;
            };
            if role_keys.contains(&(owner_name.as_str(), managed.name.as_str())) {
                continue;
            }
            let role_name = managed.name.clone();
            if delete_state {
                role::Entity::delete_by_id(managed.id).exec(self.db).await?;
                tracing::info!(role = %role_name, "Deleted managed role");
            } else {
                let mut active: role::ActiveModel = managed.into();
                active.managed = Set(false);
                active.update(self.db).await?;
                tracing::info!(role = %role_name, "Unmarked managed role");
            }
        }
        Ok(())
    }

    async fn delete_or_unmark_managed_git_host_connections_not_declared_in_state(
        &self,
        config: &StateConfiguration,
        project_name_by_id: &HashMap<ProjectId, String>,
        team_name_by_id: &HashMap<TeamId, String>,
        delete_state: bool,
    ) -> Result<(), DynError> {
        let declared: HashSet<(GitHostConnectionOwner, &str)> = config
            .git_host_connections
            .values()
            .filter_map(|c| Some((c.owner.parse().ok()?, c.name.as_str())))
            .collect();
        let managed_connections = EGitHostConnection::find()
            .filter(CGitHostConnection::Managed.eq(true))
            .all(self.db)
            .await?;
        for managed in managed_connections {
            let owner = match GitHostConnectionOwnerByDatabaseId::of(&managed) {
                GitHostConnectionOwnerByDatabaseId::Instance => GitHostConnectionOwner::Instance,
                GitHostConnectionOwnerByDatabaseId::Team(id) => {
                    let Some(name) = team_name_by_id.get(&id) else {
                        continue;
                    };
                    GitHostConnectionOwner::Team { name: name.clone() }
                }
                GitHostConnectionOwnerByDatabaseId::Project(id) => {
                    let Some(name) = project_name_by_id.get(&id) else {
                        continue;
                    };
                    GitHostConnectionOwner::Project { name: name.clone() }
                }
            };
            if declared.contains(&(owner.clone(), managed.name.as_str())) {
                continue;
            }
            let name = managed.name.clone();
            if delete_state {
                EGitHostConnection::delete_by_id(managed.id)
                    .exec(self.db)
                    .await?;
                tracing::info!(%owner, %name, "Deleted the managed connection to the Git host.");
            } else {
                let mut active: AGitHostConnection = managed.into();
                active.managed = Set(false);
                active.update(self.db).await?;
                tracing::info!(%owner, %name, "Unmarked Connection to a Managed Git Host");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod keep_set_tests {
    use super::*;

    #[test]
    fn keep_sets_track_inner_name_not_attrset_key() {
        let json = serde_json::json!({
            "users": {
                "alice-key": {
                    "username": "alice",
                    "name": "Alice",
                    "email": "alice@example.com",
                    "password_file": "/dev/null"
                }
            },
            "projects": {
                "acme-key": {
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            },
            "tasks": {
                "foo-key": {
                    "name": "main",
                    "project": "acme",
                    "display_name": "Main",
                    "repository": "https://example.com/r.git",
                    "created_by": "alice"
                }
            },
            "caches": {
                "cache-key": {
                    "name": "primary",
                    "display_name": "Primary",
                    "signing_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            },
            "api_keys": {
                "key-key": {
                    "name": "ci-runner",
                    "key_file": "/dev/null",
                    "owned_by": "alice",
                    "permissions": ["viewProject"]
                }
            }
        });
        let cfg: StateConfiguration = serde_json::from_value(json).unwrap();
        let sets = managed_keep_sets(&cfg);

        let alice = "alice".to_string();
        let alice_key = "alice-key".to_string();
        assert!(sets.usernames.contains(&alice));
        assert!(!sets.usernames.contains(&alice_key));

        let acme = "acme".to_string();
        let acme_key = "acme-key".to_string();
        assert!(sets.project_names.contains(&acme));
        assert!(!sets.project_names.contains(&acme_key));

        let main = "main".to_string();
        let foo_key = "foo-key".to_string();
        assert!(sets.task_names.contains(&main));
        assert!(!sets.task_names.contains(&foo_key));

        let primary = "primary".to_string();
        let cache_key = "cache-key".to_string();
        assert!(sets.cache_names.contains(&primary));
        assert!(!sets.cache_names.contains(&cache_key));

        let ci_runner = "ci-runner".to_string();
        let key_key = "key-key".to_string();
        assert!(sets.api_key_names.contains(&ci_runner));
        assert!(!sets.api_key_names.contains(&key_key));
    }
}

#[cfg(test)]
mod managed_git_host_connection_reconciliation_tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};
    use sea_orm::{DatabaseConnection, MockExecResult, Statement};

    struct Owners {
        acme: ProjectId,
        other: ProjectId,
        platform: TeamId,
    }

    impl Owners {
        fn new() -> Self {
            Self {
                acme: ProjectId::now_v7(),
                other: ProjectId::now_v7(),
                platform: TeamId::now_v7(),
            }
        }
    }

    fn state_listing_hook_of_acme_and_forge_of_platform_and_instance() -> StateConfiguration {
        let connection = |name: &str, owner: &str| {
            serde_json::json!({
                "name": name,
                "owner": owner,
                "git_host_type": "gitea",
                "created_by": "alice"
            })
        };
        serde_json::from_value(serde_json::json!({
            "git_host_connections": {
                "acme-hook": connection("hook", "project/acme"),
                "platform-forge": connection("forge", "team/platform"),
                "instance-forge": connection("forge", "instance"),
            }
        }))
        .unwrap()
    }

    fn managed_row(
        name: &str,
        project: Option<ProjectId>,
        owner_team: Option<TeamId>,
    ) -> MGitHostConnection {
        MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            project,
            owner_team,
            name: name.into(),
            managed: true,
            ..Default::default()
        }
    }

    async fn reconcile(db: &DatabaseConnection, owners: &Owners, delete: bool) {
        let app = StateApplicator {
            db,
            crypt_secret_file: "",
            credentials_dir: std::path::Path::new(""),
            email_enabled: false,
        };
        let project_name_by_id = HashMap::from([
            (owners.acme, "acme".to_string()),
            (owners.other, "other".to_string()),
        ]);
        let team_name_by_id = HashMap::from([(owners.platform, "platform".to_string())]);
        app.delete_or_unmark_managed_git_host_connections_not_declared_in_state(
            &state_listing_hook_of_acme_and_forge_of_platform_and_instance(),
            &project_name_by_id,
            &team_name_by_id,
            delete,
        )
        .await
        .unwrap();
    }

    fn mentions(statement: &Statement, id: GitHostConnectionId) -> bool {
        format!("{:?}", statement.values).contains(&id.to_string())
    }

    #[tokio::test]
    async fn delete_mode_deletes_managed_connections_missing_from_state_by_owner_and_name_and_keeps_listed_connection()
     {
        let owners = Owners::new();
        let listed = [
            managed_row("hook", Some(owners.acme), None),
            managed_row("forge", None, Some(owners.platform)),
            managed_row("forge", None, None),
        ];
        let removed = [
            managed_row("hook", Some(owners.other), None),
            managed_row("hook", None, Some(owners.platform)),
            managed_row("hook", None, None),
        ];
        let deleted = MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        };
        let db = mock_db()
            .append_query_results([listed.iter().chain(&removed).cloned().collect::<Vec<_>>()])
            .append_exec_results(std::iter::repeat_n(deleted, removed.len()))
            .into_connection();

        reconcile(&db, &owners, true).await;

        let deletes: Vec<Statement> = all_statements(db.into_transaction_log())
            .into_iter()
            .filter(|s| s.sql.starts_with("DELETE FROM \"git_host_connection\""))
            .collect();
        assert_eq!(deletes.len(), removed.len());
        for row in &removed {
            assert!(deletes.iter().any(|s| mentions(s, row.id)));
        }
        for row in &listed {
            assert!(!deletes.iter().any(|s| mentions(s, row.id)));
        }
    }

    #[tokio::test]
    async fn removed_connection_is_unmanaged_and_not_deleted_when_delete_mode_is_off() {
        let owners = Owners::new();
        let listed = managed_row("hook", Some(owners.acme), None);
        let removed = managed_row("old-hook", Some(owners.acme), None);
        let db = mock_db()
            .append_query_results([vec![listed.clone(), removed.clone()]])
            .append_query_results([vec![MGitHostConnection {
                managed: false,
                ..removed.clone()
            }]])
            .into_connection();

        reconcile(&db, &owners, false).await;

        let log = all_statements(db.into_transaction_log());
        assert!(!log.iter().any(|s| s.sql.starts_with("DELETE")));
        let updates: Vec<&Statement> = log
            .iter()
            .filter(|s| s.sql.starts_with("UPDATE \"git_host_connection\""))
            .collect();
        assert_eq!(updates.len(), 1);
        assert!(mentions(updates[0], removed.id));
        assert!(format!("{:?}", updates[0].values).contains("Bool(Some(false))"));
    }
}
