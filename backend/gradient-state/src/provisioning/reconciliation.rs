/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drift reconciliation: unmanage/delete state-managed rows no longer present in state.

use super::DynError;
use super::StateApplicator;
use crate::config::StateConfiguration;
use gradient_entity::*;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

/// Names of state-managed rows that must remain `managed` after reconciliation.
///
/// Each set is built from the value's `name` (or `username`) field - the same
/// field every `apply_*` writes to the DB row. Using the attrset key here
/// instead would delete or unmanage rows whose Nix-side `name = "…"` was
/// overridden away from the attrset key (e.g. `projects.foo = { name = "main"; }`).
pub(crate) struct ManagedKeepSets<'a> {
    usernames: HashSet<&'a String>,
    org_names: HashSet<&'a String>,
    project_names: HashSet<&'a String>,
    cache_names: HashSet<&'a String>,
    api_key_names: HashSet<&'a String>,
}

pub(crate) fn managed_keep_sets(config: &StateConfiguration) -> ManagedKeepSets<'_> {
    ManagedKeepSets {
        usernames: config.users.values().map(|u| &u.username).collect(),
        org_names: config.organizations.values().map(|o| &o.name).collect(),
        project_names: config.projects.values().map(|p| &p.name).collect(),
        cache_names: config.caches.values().map(|c| &c.name).collect(),
        api_key_names: config.api_keys.values().map(|k| &k.name).collect(),
    }
}

/// For each managed row not present in the state set, either delete it (if
/// `delete_state`) or flip `managed` to `false`. `name_field` names the column
/// used to compare against the state set and to log; `label` is the
/// human-readable noun for log lines.
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
    // ── unmark_removed_entities ───────────────────────────────────────────────

    pub(crate) async fn unmark_removed_entities(
        &self,
        config: &StateConfiguration,
        delete_state: bool,
    ) -> Result<(), DynError> {
        let ManagedKeepSets {
            usernames,
            org_names,
            project_names,
            cache_names,
            api_key_names,
        } = managed_keep_sets(config);
        let worker_keys: HashSet<(String, OrganizationId)> = {
            let map = self.org_lookup().await?;
            let mut set = HashSet::new();
            for worker in config.workers.values() {
                for org in &worker.organizations {
                    if let Some(peer_id) = map.get(org) {
                        set.insert((worker.worker_id.clone(), *peer_id));
                    }
                }
            }
            set
        };

        let db = self.db;

        unmark_managed!(db, user, usernames, username, delete_state, "user");
        unmark_managed!(
            db,
            organization,
            org_names,
            name,
            delete_state,
            "organization"
        );
        unmark_managed!(db, project, project_names, name, delete_state, "project");
        unmark_managed!(db, cache, cache_names, name, delete_state, "cache");
        unmark_managed!(db, api, api_key_names, name, delete_state, "API key");

        // Roles: identified by (organization, name) so we can't use the
        // single-column `unmark_managed!` helper.
        let role_keys: HashSet<(String, String)> = config
            .roles
            .values()
            .map(|r| (r.organization.clone(), r.name.clone()))
            .collect();
        let org_lookup = self.org_lookup().await?;
        let mut org_name_by_id: HashMap<OrganizationId, String> = HashMap::new();
        for (name, id) in &org_lookup {
            org_name_by_id.insert(*id, name.clone());
        }
        let managed_roles = role::Entity::find()
            .filter(role::Column::Managed.eq(true))
            .all(db)
            .await?;
        for managed in managed_roles {
            let owner_org = match managed.organization {
                Some(id) => id,
                None => continue,
            };
            let owner_name = match org_name_by_id.get(&owner_org) {
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

        let base_worker_ids: HashSet<&String> = config
            .workers
            .values()
            .filter(|w| w.base_worker)
            .map(|w| &w.worker_id)
            .collect();
        let base_workers = base_worker::Entity::find().all(db).await?;
        for bw in base_workers {
            if base_worker_ids.contains(&bw.worker_id) {
                continue;
            }
            organization_base_worker::Entity::delete_many()
                .filter(organization_base_worker::Column::BaseWorker.eq(bw.id))
                .exec(db)
                .await?;
            let worker_id = bw.worker_id.clone();
            base_worker::Entity::delete_by_id(bw.id).exec(db).await?;
            tracing::info!(worker_id, "Deleted base worker");
        }

        Ok(())
    }
}

#[cfg(test)]
mod keep_set_tests {
    use super::*;

    /// Regression: `gradient-state.nix` lets users override an entity's
    /// `name`/`username` away from the attrset key. The cleanup pass must look
    /// up DB rows by the same `name` the `apply_*` functions wrote - using the
    /// attrset key here would unmanage or delete the row we just inserted.
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
            "organizations": {
                "acme-key": {
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            },
            "projects": {
                "foo-key": {
                    "name": "main",
                    "organization": "acme",
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
                    "permissions": ["viewOrg"]
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
        assert!(sets.org_names.contains(&acme));
        assert!(!sets.org_names.contains(&acme_key));

        let main = "main".to_string();
        let foo_key = "foo-key".to_string();
        assert!(sets.project_names.contains(&main));
        assert!(!sets.project_names.contains(&foo_key));

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
