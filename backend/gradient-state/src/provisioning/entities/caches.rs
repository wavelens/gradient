/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{
    choose_row_to_update_from_declared_id_and_name, lookup_id,
    order_state_entries_with_declared_id_first, parse_optional_uuid_string_as_typed_id,
    read_credential,
};
use crate::config::*;
use anyhow::Result;
use base64::{Engine, engine::general_purpose};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::*;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
};
use gradient_types::*;
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use gradient_util::upstream_cache_credential::{
    upstream_cache_credential_plaintext_from_bearer_token,
    validate_basic_auth_and_make_upstream_cache_credential,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_caches(
        &self,
        state_caches: &HashMap<String, StateCache>,
        team_ids: &HashMap<String, TeamId>,
    ) -> Result<(), DynError> {
        let state_users = self.load_managed_accounts_by_lowercase_user_name().await?;
        let project_map = self.project_lookup().await?;
        let mut applied = Vec::with_capacity(state_caches.len());

        for state_cache in
            order_state_entries_with_declared_id_first(state_caches.values(), |c| (&c.id, &c.name))
        {
            let owner_team = state_cache
                .owner_team
                .as_deref()
                .map(|team| lookup_id(team_ids, team, "Team"))
                .transpose()?;
            let (signing_key, _) = read_credential(
                self.credentials_dir,
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

            let declared_id = parse_optional_uuid_string_as_typed_id::<CacheId>(
                "Cache",
                &state_cache.name,
                state_cache.id.as_deref(),
            )?;
            let row_with_declared_id = match declared_id {
                Some(id) => cache::Entity::find_by_id(id).one(self.db).await?,
                None => None,
            };
            let row_with_name = cache::Entity::find()
                .filter(cache::Column::Name.eq(&state_cache.name))
                .one(self.db)
                .await?;
            let existing_cache = choose_row_to_update_from_declared_id_and_name(
                "Cache",
                &state_cache.name,
                declared_id,
                row_with_declared_id,
                row_with_name,
                |cache| cache.id,
            )?;
            let created_by_id = state_users.resolve_creator_username_to_state_account_id(
                &state_cache.created_by,
                &format!("cache '{}'", state_cache.name),
                existing_cache.as_ref().map(|cache| cache.created_by),
            )?;

            let now = now();

            let cache_id = if let Some(existing) = existing_cache {
                let mut cache_model: cache::ActiveModel = existing.clone().into();
                cache_model.name = Set(state_cache.name.clone());
                cache_model.display_name = Set(state_cache.display_name.clone());
                cache_model.description = Set(state_cache.description.clone().unwrap_or_default());
                cache_model.active = Set(state_cache.active);
                cache_model.priority = Set(state_cache.priority);
                cache_model.local_priority = Set(state_cache.local_priority);
                cache_model.max_storage_gb = Set(state_cache.max_storage_gb);
                cache_model.pull_through = Set(state_cache.pull_through);
                cache_model.public_key = Set(public_key.clone());
                cache_model.private_key = Set(encrypted_signing_key.clone());
                cache_model.created_by = Set(created_by_id);
                cache_model.public = Set(state_cache.public);
                cache_model.managed = Set(true);
                cache_model.owner_team = Set(owner_team);
                cache_model.update(self.db).await?;
                tracing::info!(name = %state_cache.name, "Updated managed cache");
                existing.id
            } else {
                let cache_id = declared_id.unwrap_or_else(CacheId::now_v7);
                let cache_model = cache::Model {
                    owner_team,
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
                    pull_through: state_cache.pull_through,
                }
                .into_active_model();

                cache_model.insert(self.db).await?;
                tracing::info!(name = %state_cache.name, "Created managed cache");
                cache_id
            };
            applied.push((cache_id, created_by_id, state_cache));
        }

        for (cache_id, created_by_id, state_cache) in applied {
            self.apply_upstream_caches(cache_id, &state_cache.name, &state_cache.upstream_caches)
                .await?;

            self.apply_cache_subscriptions(cache_id, state_cache, &project_map)
                .await?;

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

            // The `created_by` user must always hold the Admin cache role. `PUT /caches` is
            // inserting this row at creation. An empty `members` list otherwise left the cache
            // without any admin.
            if state_cache.created_by.is_empty() {
                continue;
            }
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

    async fn apply_cache_subscriptions(
        &self,
        cache_id: CacheId,
        state_cache: &StateCache,
        project_map: &HashMap<String, ProjectId>,
    ) -> Result<(), DynError> {
        let existing: HashMap<ProjectId, project_cache::Model> = project_cache::Entity::find()
            .filter(project_cache::Column::Cache.eq(cache_id))
            .all(self.db)
            .await?
            .into_iter()
            .map(|row| (row.project, row))
            .collect();
        let mut declared = HashSet::new();

        for subscription in &state_cache.projects {
            let project_id = project_map
                .get(&subscription.name)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "Project '{}' not found for cache '{}'",
                        subscription.name, state_cache.name
                    )
                })?;
            declared.insert(project_id);
            let cache_signs_outputs = subscription.signs_project_outputs();

            match existing.get(&project_id) {
                Some(row)
                    if row.managed
                        && row.mode == subscription.mode
                        && row.sign_outputs == cache_signs_outputs => {}
                Some(row) => {
                    let mut active = row.clone().into_active_model();
                    active.managed = Set(true);
                    active.mode = Set(subscription.mode.clone());
                    active.sign_outputs = Set(cache_signs_outputs);
                    active.update(self.db).await?;
                }
                None => {
                    project_cache::Model {
                        project: project_id,
                        cache: cache_id,
                        mode: subscription.mode.clone(),
                        managed: true,
                        sign_outputs: cache_signs_outputs,
                        ..Default::default()
                    }
                    .into_active_model()
                    .insert(self.db)
                    .await?;
                    tracing::info!(
                        project = %subscription.name,
                        cache = %state_cache.name,
                        "Created project_cache association"
                    );
                }
            }
        }

        let stale: Vec<ProjectCacheId> = existing
            .values()
            .filter(|row| row.managed && !declared.contains(&row.project))
            .map(|row| row.id)
            .collect();
        if !stale.is_empty() {
            project_cache::Entity::delete_many()
                .filter(project_cache::Column::Id.is_in(stale))
                .exec(self.db)
                .await?;
        }
        Ok(())
    }

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

    pub(crate) async fn apply_upstream_caches(
        &self,
        cache_id: CacheId,
        cache_name: &str,
        upstream_caches: &[StateUpstream],
    ) -> Result<(), DynError> {
        ECacheUpstream::delete_many()
            .filter(CCacheUpstream::Cache.eq(cache_id))
            .exec(self.db)
            .await?;

        if upstream_caches.is_empty() {
            return Ok(());
        }

        let cache_lookup: HashMap<String, CacheId> = ECache::find()
            .all(self.db)
            .await?
            .into_iter()
            .map(|c| (c.name, c.id))
            .collect();

        for (index, upstream) in upstream_caches.iter().enumerate() {
            let record = match upstream {
                StateUpstream::Internal {
                    cache_name: upstream_cache_name,
                    display_name,
                    mode,
                    active,
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
                        active: *active,
                        ..Default::default()
                    }
                    .into_active_model()
                }
                StateUpstream::External {
                    display_name,
                    url,
                    public_key,
                    http1_only,
                    active,
                    basic_auth_user,
                    basic_auth_password_file,
                    bearer_token_file,
                } => {
                    let url = url.parse::<ValidatedHttpBaseUrl>().map_err(|error| {
                        format!(
                            "Invalid URL for upstream cache '{display_name}' in cache '{cache_name}': {error}"
                        )
                    })?;
                    let credential = self.read_upstream_cache_credential(
                        cache_name,
                        index,
                        &url,
                        basic_auth_user.as_deref(),
                        basic_auth_password_file.is_some(),
                        bearer_token_file.is_some(),
                    )?;
                    let (credential_scheme, encrypted_credential) = credential.unzip();
                    MCacheUpstream {
                        id: CacheUpstreamId::now_v7(),
                        cache: cache_id,
                        display_name: display_name.clone(),
                        mode: CacheSubscriptionMode::ReadOnly,
                        kind: cache_upstream::CacheUpstreamKind::Http,
                        url: Some(url.to_string()),
                        public_key: Some(public_key.clone()),
                        encrypted_credential,
                        credential_scheme,
                        http1_only: *http1_only,
                        active: *active,
                        ..Default::default()
                    }
                    .into_active_model()
                }
            };

            record.insert(self.db).await?;
        }

        tracing::debug!(
            count = upstream_caches.len(),
            cache = %cache_name,
            "Applied upstream caches to cache"
        );
        Ok(())
    }

    fn read_upstream_cache_credential(
        &self,
        cache_name: &str,
        index: usize,
        url: &ValidatedHttpBaseUrl,
        basic_auth_user: Option<&str>,
        upstream_cache_has_basic_auth_password_file: bool,
        sets_bearer_token_file: bool,
    ) -> Result<Option<(cache_upstream::UpstreamCredentialScheme, String)>, DynError> {
        let upstream = format!("upstream cache {index} of cache '{cache_name}'");
        let upstream_cache_credential_name_part = format!("{cache_name}_{index}");
        let read = |suffix: &str, label: &str| -> Result<String, DynError> {
            let (contents, path) = read_credential(
                self.credentials_dir,
                "cache_upstream",
                &upstream_cache_credential_name_part,
                suffix,
                label,
            )?;
            let contents = contents.trim();
            if contents.is_empty() {
                return Err(format!("The {label} {path} is empty.").into());
            }
            Ok(contents.to_owned())
        };
        let error_when_upstream_cache_url_is_plain_http = || -> Result<(), DynError> {
            if url.as_str().starts_with("http://") {
                return Err(format!("Credentials for {upstream} require an https URL.").into());
            }
            Ok(())
        };

        let (scheme, payload) = match (
            basic_auth_user,
            upstream_cache_has_basic_auth_password_file,
            sets_bearer_token_file,
        ) {
            (None, false, false) => return Ok(None),
            (Some(user), true, false) => {
                error_when_upstream_cache_url_is_plain_http()?;
                let password = read("password", "password file of the upstream cache")?;
                (
                    cache_upstream::UpstreamCredentialScheme::Basic,
                    validate_basic_auth_and_make_upstream_cache_credential(user, &password)
                        .map_err(|refusal| format!("{upstream}: {refusal}"))?,
                )
            }
            (None, false, true) => {
                error_when_upstream_cache_url_is_plain_http()?;
                let token = read("bearer_token", "upstream cache bearer token file")?;
                (
                    cache_upstream::UpstreamCredentialScheme::Bearer,
                    upstream_cache_credential_plaintext_from_bearer_token(&token)
                        .map_err(|refusal| format!("{upstream}: {refusal}"))?,
                )
            }
            _ => {
                return Err(format!(
                    "The credential fields of {upstream} conflict. {EXTERNAL_HTTP_UPSTREAM_CACHE_CREDENTIAL_FIELD_COMBINATION_ERROR_MESSAGE}"
                )
                .into());
            }
        };
        let encrypted = self.encrypt_to_b64(&payload, &format!("{upstream} credentials"))?;
        Ok(Some((scheme, encrypted)))
    }

    pub(crate) async fn apply_cache_roles_and_members(
        &self,
        cache_id: CacheId,
        cache_name: &str,
        roles: &[StateCacheRoleEntry],
        members: &[StateCacheMemberEntry],
    ) -> Result<(), DynError> {
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
                let p = gradient_types::permissions::CachePermission::from_wire_name(wire)
                    .ok_or_else(|| {
                        format!(
                            "Cache '{}' role '{}' references unknown permission '{}'",
                            cache_name, entry.name, wire
                        )
                    })?;
                perms.push(p);
            }
            let mask = gradient_types::permissions::cache_mask_with_view_permission_set(
                gradient_types::permissions::cache_mask_from(&perms),
            );

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

        let state_users = self.load_managed_accounts_by_lowercase_user_name().await?;
        let mut declared_members: HashSet<UserId> = HashSet::new();
        for entry in members {
            let Some(user_id) = state_users
                .resolve_username_to_user_id(&entry.user, &format!("cache '{cache_name}' member"))
            else {
                continue;
            };

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

        let existing_members = ECacheUser::find()
            .filter(CCacheUser::Cache.eq(cache_id))
            .all(self.db)
            .await?;

        for row in existing_members {
            if !declared_members.contains(&row.user) {
                // `cache_user` rows are carrying no managed flag. Every member missing from the
                // declared list is removed while the cache is managed.
                ECacheUser::delete_by_id(row.id).exec(self.db).await?;
                tracing::info!(cache = %cache_name, user = %row.user, "Removed cache member no longer in state");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_types::permissions::{CachePermission, cache_mask_grants};
    use std::path::Path;

    #[tokio::test]
    async fn a_declared_cache_role_always_views_its_cache() {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let cache = rows::cache()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let transaction = sea_orm::TransactionTrait::begin(&connection).await.unwrap();
        let app = StateApplicator {
            db: &transaction,
            crypt_secret_file: "",
            credentials_dir: Path::new(""),
            email_enabled: false,
            user_url_destination_allowlist: &Default::default(),
        };

        app.apply_cache_roles_and_members(
            cache.id,
            &cache.name,
            &[StateCacheRoleEntry {
                name: "uploader".into(),
                permissions: vec!["writeStore".into()],
            }],
            &[],
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let role = db
            .rows::<ECacheRole>()
            .await
            .into_iter()
            .find(|role| role.name == "uploader")
            .expect("the declared cache role is written");
        assert!(cache_mask_grants(
            role.permission,
            CachePermission::ViewCache
        ));
        assert!(cache_mask_grants(
            role.permission,
            CachePermission::WriteStore
        ));
        db.finish().await;
    }

    #[tokio::test]
    async fn applying_server_state_config_marks_declared_cache_subscriptions_managed_removes_undeclared_managed_and_keeps_hand_made_subscriptions()
     {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let cache = rows::cache()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let mut project_map = HashMap::new();
        for name in ["taken-over", "added", "dropped", "hand-made"] {
            let project = rows::project_with_id(ProjectId::now_v7(), name)
                .into_active_model()
                .insert(&connection)
                .await
                .unwrap();
            project_map.insert(name.to_string(), project.id);
        }
        for (name, managed) in [
            ("taken-over", false),
            ("dropped", true),
            ("hand-made", false),
        ] {
            project_cache::Model {
                project: project_map[name],
                cache: cache.id,
                managed,
                ..Default::default()
            }
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        }
        let state_cache: StateCache = serde_json::from_value(serde_json::json!({
            "name": cache.name,
            "display_name": "Test Cache",
            "signing_key_file": "/dev/null",
            "public": false,
            "created_by": "test",
            "projects": [
                { "name": "taken-over", "mode": "ReadOnly", "sign_outputs": false },
                { "name": "added" }
            ]
        }))
        .unwrap();
        let transaction = sea_orm::TransactionTrait::begin(&connection).await.unwrap();
        let app = StateApplicator {
            db: &transaction,
            crypt_secret_file: "",
            credentials_dir: Path::new(""),
            email_enabled: false,
            user_url_destination_allowlist: &Default::default(),
        };

        app.apply_cache_subscriptions(cache.id, &state_cache, &project_map)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let rows: HashMap<ProjectId, project_cache::Model> = db
            .rows::<EProjectCache>()
            .await
            .into_iter()
            .map(|row| (row.project, row))
            .collect();
        assert_eq!(rows.len(), 3);
        let taken_over = &rows[&project_map["taken-over"]];
        assert!(taken_over.managed);
        assert_eq!(taken_over.mode, CacheSubscriptionMode::ReadOnly);
        assert!(!taken_over.sign_outputs);
        let added = &rows[&project_map["added"]];
        assert!(added.managed);
        assert_eq!(added.mode, CacheSubscriptionMode::ReadWrite);
        assert!(added.sign_outputs);
        assert!(!rows[&project_map["hand-made"]].managed);
        db.finish().await;
    }

    #[tokio::test]
    async fn upstream_url_with_trailing_slash_is_normalised_for_project_lookup() {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let cache = rows::cache()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let project = rows::project_with_id(ProjectId::now_v7(), "subscriber")
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        project_cache::Model {
            project: project.id,
            cache: cache.id,
            ..Default::default()
        }
        .into_active_model()
        .insert(&connection)
        .await
        .unwrap();
        let transaction = sea_orm::TransactionTrait::begin(&connection).await.unwrap();
        let app = StateApplicator {
            db: &transaction,
            crypt_secret_file: "",
            credentials_dir: Path::new(""),
            email_enabled: false,
            user_url_destination_allowlist: &Default::default(),
        };

        app.apply_upstream_caches(
            cache.id,
            &cache.name,
            &[StateUpstream::External {
                display_name: "Attic".into(),
                url: "https://Attic.example/".into(),
                public_key: "attic:key".into(),
                http1_only: false,
                active: true,
                basic_auth_user: None,
                basic_auth_password_file: None,
                bearer_token_file: None,
            }],
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let found = gradient_db::caches::upstream::upstream_urls_for_projects(
            &connection,
            &format!("'{}'", project.id),
        )
        .await
        .unwrap();
        assert_eq!(found, HashSet::from(["https://attic.example".to_owned()]));
        db.finish().await;
    }

    const TEST_UPSTREAM_CREDENTIALS_ENCRYPTION_KEY_SECRET: &str =
        "0123456789abcdef0123456789abcdef";

    fn create_temp_credentials_directory_with_encryption_key_and_files(
        files: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gradient-state-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("crypt"),
            TEST_UPSTREAM_CREDENTIALS_ENCRYPTION_KEY_SECRET,
        )
        .unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        dir
    }

    fn external_upstream(
        url: &str,
        basic_auth_user: Option<&str>,
        basic_auth_password_file: Option<&str>,
        bearer_token_file: Option<&str>,
    ) -> StateUpstream {
        StateUpstream::External {
            display_name: "Private".into(),
            url: url.into(),
            public_key: "private:key".into(),
            http1_only: false,
            active: true,
            basic_auth_user: basic_auth_user.map(str::to_owned),
            basic_auth_password_file: basic_auth_password_file.map(str::to_owned),
            bearer_token_file: bearer_token_file.map(str::to_owned),
        }
    }

    async fn create_test_database_and_apply_upstream_caches(
        credentials_dir: &Path,
        upstreams: &[StateUpstream],
    ) -> (TestDatabase, Result<(), DynError>) {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let cache = rows::cache()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let crypt_file = credentials_dir.join("crypt");
        let transaction = sea_orm::TransactionTrait::begin(&connection).await.unwrap();
        let app = StateApplicator {
            db: &transaction,
            crypt_secret_file: crypt_file.to_str().unwrap(),
            credentials_dir,
            email_enabled: false,
            user_url_destination_allowlist: &Default::default(),
        };
        let result = app
            .apply_upstream_caches(cache.id, &cache.name, upstreams)
            .await;
        if result.is_ok() {
            transaction.commit().await.unwrap();
        }
        (db, result)
    }

    fn decrypt(stored: &str) -> String {
        let bytes = general_purpose::STANDARD.decode(stored).unwrap();
        String::from_utf8(
            crypter::decrypt_with_password(
                TEST_UPSTREAM_CREDENTIALS_ENCRYPTION_KEY_SECRET.as_bytes(),
                bytes,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn state_apply_stores_encrypted_upstream_cache_credentials_read_from_password_and_token_files()
     {
        let dir = create_temp_credentials_directory_with_encryption_key_and_files(&[
            (
                "gradient_cache_upstream_test-cache_0_password",
                "s3cret-pw\n",
            ),
            (
                "gradient_cache_upstream_test-cache_1_bearer_token",
                "t0ken\n",
            ),
        ]);
        let (db, result) = create_test_database_and_apply_upstream_caches(
            &dir,
            &[
                external_upstream(
                    "https://basic.example",
                    Some("alice"),
                    Some("/run/secrets/pw"),
                    None,
                ),
                external_upstream("https://bearer.example", None, None, Some("/run/secrets/t")),
            ],
        )
        .await;
        result.unwrap();

        let rows: HashMap<String, cache_upstream::Model> = db
            .rows::<ECacheUpstream>()
            .await
            .into_iter()
            .map(|row| (row.url.clone().unwrap(), row))
            .collect();
        let basic = &rows["https://basic.example"];
        assert_eq!(
            basic.credential_scheme,
            Some(cache_upstream::UpstreamCredentialScheme::Basic)
        );
        assert_eq!(
            decrypt(basic.encrypted_credential.as_deref().unwrap()),
            "alice:s3cret-pw"
        );
        let bearer = &rows["https://bearer.example"];
        assert_eq!(
            bearer.credential_scheme,
            Some(cache_upstream::UpstreamCredentialScheme::Bearer)
        );
        assert_eq!(
            decrypt(bearer.encrypted_credential.as_deref().unwrap()),
            "t0ken"
        );
        db.finish().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn test_state_apply_fails_with_credential_file_path_when_upstream_cache_credential_is_missing()
     {
        let dir = create_temp_credentials_directory_with_encryption_key_and_files(&[]);
        let (db, result) = create_test_database_and_apply_upstream_caches(
            &dir,
            &[external_upstream(
                "https://basic.example",
                Some("alice"),
                Some("/run/secrets/pw"),
                None,
            )],
        )
        .await;

        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("gradient_cache_upstream_test-cache_0_password"),
            "got: {error}"
        );
        db.finish().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn state_apply_rejects_http_upstream_cache_with_credential_and_error_omits_secret_and_no_row_stored()
     {
        let dir = create_temp_credentials_directory_with_encryption_key_and_files(&[(
            "gradient_cache_upstream_test-cache_0_bearer_token",
            "t0ken",
        )]);
        let (db, result) = create_test_database_and_apply_upstream_caches(
            &dir,
            &[external_upstream(
                "http://plain.example",
                None,
                None,
                Some("/run/secrets/t"),
            )],
        )
        .await;

        let error = result.unwrap_err().to_string();
        assert!(error.contains("https"), "got: {error}");
        assert!(!error.contains("t0ken"), "got: {error}");
        assert!(db.rows::<ECacheUpstream>().await.is_empty());
        db.finish().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refuses_upstream_cache_url_with_userinfo_or_query() {
        for url in [
            "https://alice:pw@private.example",
            "https://private.example?token=abc",
        ] {
            let (db, result) = create_test_database_and_apply_upstream_caches(
                Path::new(""),
                &[external_upstream(url, None, None, None)],
            )
            .await;
            assert!(result.is_err(), "{url} was accepted");
            db.finish().await;
        }
    }
}
