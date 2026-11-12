/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{parse_api_key_hash, read_credential};
use crate::config::*;
use anyhow::Result;
use gradient_entity::*;
use gradient_types::permissions::{
    CachePermission, Permission, PermissionMask, cache_mask_from, mask_from,
    mask_with_view_project_permission,
};
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_api_keys(
        &self,
        state_api_keys: &HashMap<String, StateApiKey>,
    ) -> Result<(), DynError> {
        let user_lookup = self.user_lookup().await?;
        let project_lookup = self.project_lookup().await?;
        let now = now();

        for state_api_key in state_api_keys.values() {
            let owned_by_id = user_lookup
                .get(&state_api_key.owned_by)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "User '{}' not found for API key '{}'",
                        state_api_key.owned_by, state_api_key.name
                    )
                })?;

            let mask = project_permission_bitmask_from_wire_names(state_api_key)?;
            let cache_mask = parse_cache_permission_names_to_bitmask(state_api_key)?;
            if mask == 0 && cache_mask == 0 {
                return Err(format!(
                    "Declare at least one permission or cache permission for API key '{}'.",
                    state_api_key.name
                )
                .into());
            }

            let pinned_project = match &state_api_key.project {
                None => None,
                Some(name) => Some(project_lookup.get(name).copied().ok_or_else(|| {
                    format!(
                        "Project '{}' referenced by API key '{}' not found",
                        name, state_api_key.name
                    )
                })?),
            };

            let (key_value, key_path) = read_credential(
                self.credentials_dir,
                "api",
                &state_api_key.name,
                "key",
                "API key file",
            )?;
            let key_hash = parse_api_key_hash(&key_value, &key_path)?;

            let existing_api_key = api::Entity::find()
                .filter(api::Column::Name.eq(&state_api_key.name))
                .filter(api::Column::OwnedBy.eq(owned_by_id))
                .one(self.db)
                .await?;

            if let Some(api_key_model) = existing_api_key {
                warn_if_declared_permissions_remove_use_superuser_rights(
                    state_api_key,
                    api_key_model.permission,
                    mask,
                );
                let mut api_key: api::ActiveModel = api_key_model.into();
                api_key.key = Set(key_hash);
                api_key.managed = Set(true);
                api_key.permission = Set(mask);
                api_key.cache_permission = Set(cache_mask);
                api_key.project = Set(pinned_project);
                api_key.update(self.db).await?;
                tracing::info!(name = %state_api_key.name, "Updated managed API key");
            } else {
                let api_key_model = api::Model {
                    id: ApiId::now_v7(),
                    owned_by: owned_by_id,
                    name: state_api_key.name.clone(),
                    key: key_hash,
                    last_used_at: now,
                    created_at: now,
                    managed: true,
                    permission: mask,
                    cache_permission: cache_mask,
                    project: pinned_project,
                    ..Default::default()
                }
                .into_active_model();

                api_key_model.insert(self.db).await?;
                tracing::info!(name = %state_api_key.name, "Created managed API key");
            }
        }

        Ok(())
    }
}

fn warn_if_declared_permissions_remove_use_superuser_rights(
    state_api_key: &StateApiKey,
    stored_mask: PermissionMask,
    declared_mask: PermissionMask,
) {
    let use_superuser_rights = Permission::UseSuperuserRights.bit();
    if stored_mask & use_superuser_rights != 0 && declared_mask & use_superuser_rights == 0 {
        tracing::warn!(
            name = %state_api_key.name,
            "Declared permissions that lack useSuperuserRights remove superuser access from this key. Add useSuperuserRights to the permissions list to keep that access."
        );
    }
}

fn project_permission_bitmask_from_wire_names(
    state_api_key: &StateApiKey,
) -> Result<PermissionMask, DynError> {
    let mut perms = Vec::with_capacity(state_api_key.permissions.len());
    for wire in &state_api_key.permissions {
        perms.push(Permission::from_wire_name(wire).ok_or_else(|| {
            format!(
                "API key '{}' references unknown permission '{}'",
                state_api_key.name, wire
            )
        })?);
    }
    Ok(if perms.is_empty() {
        0
    } else {
        mask_with_view_project_permission(mask_from(&perms))
    })
}

fn parse_cache_permission_names_to_bitmask(
    state_api_key: &StateApiKey,
) -> Result<PermissionMask, DynError> {
    let mut perms = Vec::with_capacity(state_api_key.cache_permissions.len());
    for wire in &state_api_key.cache_permissions {
        perms.push(CachePermission::from_wire_name(wire).ok_or_else(|| {
            format!(
                "API key '{}' references unknown cache permission '{}'",
                state_api_key.name, wire
            )
        })?);
    }
    Ok(cache_mask_from(&perms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn state_api_key_cache_permissions_are_provisioned() {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        let owner = rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let credentials_dir =
            std::env::temp_dir().join(format!("gradient-state-{}", ApiId::now_v7()));
        std::fs::create_dir_all(&credentials_dir).unwrap();
        std::fs::write(
            credentials_dir.join("gradient_api_ci_key"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
        let app = StateApplicator {
            db: &connection,
            crypt_secret_file: "",
            credentials_dir: &credentials_dir,
            email_enabled: false,
        };

        let applied = app
            .apply_api_keys(&HashMap::from([(
                "ci".to_string(),
                StateApiKey {
                    name: "ci".into(),
                    key_file: String::new(),
                    owned_by: owner.username.clone(),
                    permissions: Vec::new(),
                    cache_permissions: vec!["readStore".into(), "writeStore".into()],
                    project: None,
                },
            )]))
            .await;
        std::fs::remove_dir_all(&credentials_dir).unwrap();
        applied.unwrap();

        let key = db
            .rows::<api::Entity>()
            .await
            .into_iter()
            .find(|key| key.name == "ci")
            .expect("the declared key is written");
        assert_eq!(key.permission, 0);
        assert_eq!(
            key.cache_permission,
            cache_mask_from(&[CachePermission::ReadStore, CachePermission::WriteStore])
        );
        db.finish().await;
    }
}
