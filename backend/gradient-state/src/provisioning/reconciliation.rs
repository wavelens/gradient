/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DynError;
use super::StateApplicator;
use crate::config::StateConfiguration;
use gradient_entity::*;
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
        let worker_keys: HashSet<(String, ProjectId)> = {
            let map = self.project_lookup().await?;
            let mut set = HashSet::new();
            for worker in config.workers.values() {
                for project in &worker.projects {
                    if let Some(peer_id) = map.get(project) {
                        set.insert((worker.worker_id.clone(), *peer_id));
                    }
                }
            }
            set
        };

        let db = self.db;

        unmark_managed!(db, user, usernames, username, delete_state, "user");
        unmark_managed!(db, project, project_names, name, delete_state, "project");
        unmark_managed!(db, task, task_names, name, delete_state, "task");
        unmark_managed!(db, cache, cache_names, name, delete_state, "cache");
        unmark_managed!(db, api, api_key_names, name, delete_state, "API key");
        unmark_managed!(db, team, team_names, name, delete_state, "team");

        let role_keys: HashSet<(String, String)> = config
            .roles
            .values()
            .map(|r| (r.project.clone(), r.name.clone()))
            .collect();
        let project_lookup = self.project_lookup().await?;
        let mut project_name_by_id: HashMap<ProjectId, String> = HashMap::new();
        for (name, id) in &project_lookup {
            project_name_by_id.insert(*id, name.clone());
        }
        let managed_roles = role::Entity::find()
            .filter(role::Column::Managed.eq(true))
            .all(db)
            .await?;
        for managed in managed_roles {
            let owner_project = match managed.project {
                Some(id) => id,
                None => continue,
            };
            let owner_name = match project_name_by_id.get(&owner_project) {
                Some(n) => n.clone(),
                None => continue,
            };
            let key = (owner_name, managed.name.clone());
            if role_keys.contains(&key) {
                continue;
            }
            let role_id = managed.id;
            let role_name = managed.name.clone();
            if delete_state {
                role::Entity::delete_by_id(role_id).exec(db).await?;
                tracing::info!(role = %role_name, "Deleted managed role");
            } else {
                let mut active: role::ActiveModel = managed.into();
                active.managed = Set(false);
                active.update(db).await?;
                tracing::info!(role = %role_name, "Unmarked managed role");
            }
        }

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
