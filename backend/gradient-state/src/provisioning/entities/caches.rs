/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{lookup_id, read_credential};
use crate::config::*;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
};
use gradient_types::*;
use anyhow::Result;
use base64::{Engine, engine::general_purpose};
use gradient_entity::organization_cache::CacheSubscriptionMode;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

impl<'a> StateApplicator<'a> {
    // ── apply_caches ──────────────────────────────────────────────────────────

    pub(crate) async fn apply_caches(
        &self,
        state_caches: &HashMap<String, StateCache>,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;
        let org_map = self.org_lookup().await?;

        for state_cache in state_caches.values() {
            let (signing_key, _) = read_credential(
                "cache",
                &state_cache.name,
                "signing_key",
                "signing key file",
            )?;
            let signing_key = signing_key.trim();

            let key_bytes = general_purpose::STANDARD.decode(signing_key).map_err(|e| {
                format!(
                    "Signing key for cache '{}' is not a valid base64 encoded string: {}",
                    state_cache.name, e
                )
            })?;

            if key_bytes.len() < 32 {
                return Err(format!(
                    "Signing key for cache '{}' is too short (expected at least 32 bytes)",
                    state_cache.name
                )
                .into());
            }

            let public_key = general_purpose::STANDARD.encode(&key_bytes[key_bytes.len() - 32..]);

            let encrypted_signing_key = self.encrypt_to_b64(
                signing_key,
                &format!("signing key for cache '{}'", state_cache.name),
            )?;

            let created_by_id = lookup_id(&user_map, &state_cache.created_by, "User")?;

            let existing_cache = cache::Entity::find()
                .filter(cache::Column::Name.eq(&state_cache.name))
                .one(self.db)
                .await?;

            let now = now();

            let cache_id = if let Some(existing) = existing_cache {
                let mut cache_model: cache::ActiveModel = existing.clone().into();
                cache_model.display_name = Set(state_cache.display_name.clone());
                cache_model.description = Set(state_cache.description.clone().unwrap_or_default());
                cache_model.active = Set(state_cache.active);
                cache_model.priority = Set(state_cache.priority);
                cache_model.local_priority = Set(state_cache.local_priority);
                cache_model.max_storage_gb = Set(state_cache.max_storage_gb);
                cache_model.public_key = Set(public_key.clone());
                cache_model.private_key = Set(encrypted_signing_key.clone());
                cache_model.created_by = Set(created_by_id);
                cache_model.public = Set(state_cache.public);
                cache_model.managed = Set(true);
                cache_model.update(self.db).await?;
                tracing::info!(name = %state_cache.name, "Updated managed cache");
                existing.id
            } else {
                let cache_id = CacheId::now_v7();
                let cache_model = cache::Model {
                    id: cache_id,
                    name: state_cache.name.clone(),
                    display_name: state_cache.display_name.clone(),
                    description: state_cache.description.clone().unwrap_or_default(),
                    active: state_cache.active,
                    priority: state_cache.priority,
                    local_priority: state_cache.local_priority,
                    public_key,
                    private_key: encrypted_signing_key,
                    public: state_cache.public,
                    created_by: created_by_id,
                    created_at: now,
                    managed: true,
                    max_storage_gb: state_cache.max_storage_gb,
                }
                .into_active_model();

                cache_model.insert(self.db).await?;
                tracing::info!(name = %state_cache.name, "Created managed cache");
                cache_id
            };

            self.apply_cache_upstreams(cache_id, &state_cache.name, &state_cache.upstreams)
                .await?;

            for org_name in &state_cache.organizations {
                let org_id = org_map.get(org_name).copied().ok_or_else(|| {
                    format!(
                        "Organization '{}' not found for cache '{}'",
                        org_name, state_cache.name
                    )
                })?;

                let existing_association = organization_cache::Entity::find()
                    .filter(organization_cache::Column::Organization.eq(org_id))
                    .filter(organization_cache::Column::Cache.eq(cache_id))
                    .one(self.db)
                    .await?;

                if existing_association.is_none() {
                    let org_cache_model = organization_cache::Model {
                        id: OrganizationCacheId::now_v7(),
                        organization: org_id,
                        cache: cache_id,
                        mode: organization_cache::CacheSubscriptionMode::ReadWrite,
                    }
                    .into_active_model();

                    org_cache_model.insert(self.db).await?;
                    tracing::info!(
                        organization = %org_name,
                        cache = %state_cache.name,
                        "Created organization_cache association"
                    );
                }
            }

            self.apply_cache_roles_and_members(
                cache_id,
                &state_cache.name,
                &state_cache.roles,
                &state_cache.members,
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to apply roles/members for cache '{}': {}",
                    state_cache.name, e
                )
            })?;

            // Always guarantee the `created_by` user has the Admin cache role.
            // The API path (`PUT /caches`) inserts this row at creation time;
            // state-managed provisioning was leaving it out, so a config with
            // an empty `members` list ended up with no admin at all - even
            // the listed creator could not load the cache.
            self.ensure_cache_creator_admin(cache_id, created_by_id, &state_cache.name)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to ensure cache creator is admin for '{}': {}",
                        state_cache.name, e
                    )
                })?;
        }

        Ok(())
    }

    /// Idempotently insert a `cache_user` row pinning `created_by_id` to the
    /// Admin built-in role. Skips when the user already has any membership in
    /// this cache (state-declared members win - we don't overwrite their
    /// role even if they're the creator).
    pub(crate) async fn ensure_cache_creator_admin(
        &self,
        cache_id: CacheId,
        created_by_id: UserId,
        cache_name: &str,
    ) -> Result<(), DynError> {
        let existing = ECacheUser::find()
            .filter(CCacheUser::Cache.eq(cache_id))
            .filter(CCacheUser::User.eq(created_by_id))
            .one(self.db)
            .await?;
        if existing.is_some() {
            return Ok(());
        }
        let active = MCacheUser {
            id: CacheUserId::now_v7(),
            cache: cache_id,
            user: created_by_id,
            role: BASE_CACHE_ROLE_ADMIN_ID,
        }
        .into_active_model();

        active.insert(self.db).await?;
        tracing::info!(
            cache = %cache_name,
            "Added cache creator as Admin (state-managed cache)"
        );
        Ok(())
    }

    pub(crate) async fn apply_cache_upstreams(
        &self,
        cache_id: CacheId,
        cache_name: &str,
        upstreams: &[StateUpstream],
    ) -> Result<(), DynError> {
        ECacheUpstream::delete_many()
            .filter(CCacheUpstream::Cache.eq(cache_id))
            .exec(self.db)
            .await?;

        if upstreams.is_empty() {
            return Ok(());
        }

        let cache_lookup: HashMap<String, CacheId> = ECache::find()
            .all(self.db)
            .await?
            .into_iter()
            .map(|c| (c.name, c.id))
            .collect();

        for upstream in upstreams {
            let record = match upstream {
                StateUpstream::Internal {
                    cache_name: upstream_cache_name,
                    display_name,
                    mode,
                } => {
                    let upstream_id = *cache_lookup.get(upstream_cache_name).ok_or_else(|| {
                        format!(
                            "Cache '{}' not found for upstream of cache '{}'",
                            upstream_cache_name, cache_name
                        )
                    })?;
                    let name = display_name
                        .clone()
                        .unwrap_or_else(|| upstream_cache_name.clone());
                    MCacheUpstream {
                        id: CacheUpstreamId::now_v7(),
                        cache: cache_id,
                        display_name: name,
                        mode: mode.clone(),
                        kind: cache_upstream::CacheUpstreamKind::Internal,
                        upstream_cache: Some(upstream_id),
                        ..Default::default()
                    }
                    .into_active_model()
                }
                StateUpstream::External {
                    display_name,
                    url,
                    public_key,
                } => MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: cache_id,
                    display_name: display_name.clone(),
                    mode: CacheSubscriptionMode::ReadOnly,
                    kind: cache_upstream::CacheUpstreamKind::Http,
                    url: Some(url.clone()),
                    public_key: Some(public_key.clone()),
                    ..Default::default()
                }
                .into_active_model(),
            };

            record.insert(self.db).await?;
        }

        tracing::debug!(
            count = upstreams.len(),
            cache = %cache_name,
            "Applied upstreams to cache"
        );
        Ok(())
    }

    // ── apply_cache_roles_and_members ─────────────────────────────────────────

    pub(crate) async fn apply_cache_roles_and_members(
        &self,
        cache_id: CacheId,
        cache_name: &str,
        roles: &[StateCacheRoleEntry],
        members: &[StateCacheMemberEntry],
    ) -> Result<(), DynError> {
        // (a) Custom cache roles
        let mut declared_role_names: HashSet<String> = HashSet::new();
        for entry in roles {
            if matches!(entry.name.as_str(), "Admin" | "Write" | "View") {
                return Err(format!(
                    "Cache '{}' role '{}' collides with a built-in cache role",
                    cache_name, entry.name
                )
                .into());
            }

            let mut perms = Vec::with_capacity(entry.permissions.len());
            for wire in &entry.permissions {
                let p =
                    gradient_db::permissions::CachePermission::from_wire_name(wire).ok_or_else(|| {
                        format!(
                            "Cache '{}' role '{}' references unknown permission '{}'",
                            cache_name, entry.name, wire
                        )
                    })?;
                perms.push(p);
            }
            let mask = gradient_db::permissions::cache_mask_from(&perms);

            let existing = ECacheRole::find()
                .filter(CCacheRole::Cache.eq(cache_id))
                .filter(CCacheRole::Name.eq(&entry.name))
                .one(self.db)
                .await?;

            if let Some(row) = existing {
                let mut active: ACacheRole = row.into();
                active.permission = Set(mask);
                active.managed = Set(true);
                active.update(self.db).await?;
                tracing::info!(cache = %cache_name, role = %entry.name, "Updated managed cache role");
            } else {
                let active = MCacheRole {
                    id: RoleId::now_v7(),
                    name: entry.name.clone(),
                    cache: Some(cache_id),
                    permission: mask,
                    managed: true,
                }
                .into_active_model();

                active.insert(self.db).await?;
                tracing::info!(cache = %cache_name, role = %entry.name, "Created managed cache role");
            }

            declared_role_names.insert(entry.name.clone());
        }

        // (b) Members
        let user_map = self.user_lookup().await?;
        let mut declared_members: HashSet<UserId> = HashSet::new();
        for entry in members {
            let user_id = user_map.get(&entry.user).copied().ok_or_else(|| {
                format!("Cache '{}' member '{}' not found", cache_name, entry.user)
            })?;

            let role_id = match entry.role.as_str() {
                "Admin" => BASE_CACHE_ROLE_ADMIN_ID,
                "Write" => BASE_CACHE_ROLE_WRITE_ID,
                "View" => BASE_CACHE_ROLE_VIEW_ID,
                name => {
                    ECacheRole::find()
                        .filter(CCacheRole::Cache.eq(cache_id))
                        .filter(CCacheRole::Name.eq(name))
                        .one(self.db)
                        .await?
                        .ok_or_else(|| {
                            format!(
                                "Cache '{}' member '{}' references unknown role '{}'",
                                cache_name, entry.user, name
                            )
                        })?
                        .id
                }
            };

            let existing = ECacheUser::find()
                .filter(CCacheUser::Cache.eq(cache_id))
                .filter(CCacheUser::User.eq(user_id))
                .one(self.db)
                .await?;

            if let Some(row) = existing {
                if row.role != role_id {
                    let mut active: ACacheUser = row.into();
                    active.role = Set(role_id);
                    active.update(self.db).await?;
                    tracing::info!(cache = %cache_name, user = %entry.user, "Updated cache member role");
                }
            } else {
                let active = MCacheUser {
                    id: CacheUserId::now_v7(),
                    cache: cache_id,
                    user: user_id,
                    role: role_id,
                }
                .into_active_model();

                active.insert(self.db).await?;
                tracing::info!(cache = %cache_name, user = %entry.user, "Added cache member");
            }

            declared_members.insert(user_id);
        }

        // (c) Drift reconciliation - remove managed roles not in declared set
        let managed_roles = ECacheRole::find()
            .filter(CCacheRole::Cache.eq(cache_id))
            .filter(CCacheRole::Managed.eq(true))
            .all(self.db)
            .await?;

        let mut roles_to_delete: Vec<RoleId> = Vec::new();
        for row in &managed_roles {
            if !declared_role_names.contains(&row.name) {
                roles_to_delete.push(row.id);
            }
        }

        if !roles_to_delete.is_empty() {
            // Delete cache_user rows referencing these roles first (FK Restrict)
            for &role_id in &roles_to_delete {
                ECacheUser::delete_many()
                    .filter(CCacheUser::Cache.eq(cache_id))
                    .filter(CCacheUser::Role.eq(role_id))
                    .exec(self.db)
                    .await?;
                ECacheRole::delete_by_id(role_id).exec(self.db).await?;
                tracing::info!(cache = %cache_name, %role_id, "Deleted managed cache role no longer in state");
            }
        }

        // Remove cache_user rows for declared members no longer in config
        let existing_members = ECacheUser::find()
            .filter(CCacheUser::Cache.eq(cache_id))
            .all(self.db)
            .await?;

        for row in existing_members {
            if !declared_members.contains(&row.user) {
                // Only remove members that were on state-managed roles (custom or builtin used by state).
                // Since we don't track per-row "managed" on cache_user, we conservatively
                // remove all members not in the declared list when the cache is managed.
                ECacheUser::delete_by_id(row.id).exec(self.db).await?;
                tracing::info!(cache = %cache_name, user = %row.user, "Removed cache member no longer in state");
            }
        }

        Ok(())
    }
}
