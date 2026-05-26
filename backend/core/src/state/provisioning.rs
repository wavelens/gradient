/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    StateAction, StateApiKey, StateCache, StateCacheMemberEntry, StateCacheRoleEntry,
    StateConfiguration, StateFlakeInputOverride, StateIntegration, StateOrganization,
    StateOrgMemberEntry, StateProject, StateRole, StateTrigger, StateUpstream, StateUser,
    StateWorker,
};
use crate::ci::actions::encrypt_secret_with_file;
use crate::ci::{ForgeType, GITHUB_APP_INTEGRATION_NAME, IntegrationKind};
use crate::types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID, BASE_ROLE_ADMIN_ID,
    BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
};
use crate::types::input::load_secret_bytes;
use crate::types::triggers::TriggerConfig;
use crate::types::*;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose};
use entity::organization_cache::CacheSubscriptionMode;
use entity::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set,
};
use ssh_key::PrivateKey;
use std::collections::{HashMap, HashSet};
use std::fs;

type DynError = Box<dyn std::error::Error>;

/// Org membership declared in state for a user who did not exist at apply
/// time. Drained per-username when the user is later registered or signs
/// in via OIDC for the first time.
#[derive(Debug, Clone)]
pub struct PendingOrgMembership {
    pub organization: OrganizationId,
    pub role: RoleId,
}

pub type PendingOrgMemberships = HashMap<String, Vec<PendingOrgMembership>>;

// ── Entry point ───────────────────────────────────────────────────────────────

pub(super) async fn apply_state_to_database(
    db: &DatabaseConnection,
    config: &StateConfiguration,
    crypt_secret_file: &str,
    delete_state: bool,
    email_enabled: bool,
) -> Result<PendingOrgMemberships, DynError> {
    tracing::info!("Applying state to database");

    let app = StateApplicator {
        db,
        crypt_secret_file,
        email_enabled,
    };

    let mut pending: PendingOrgMemberships = HashMap::new();

    app.apply_users(&config.users).await?;
    app.apply_organizations_without_members(&config.organizations)
        .await?;
    app.apply_roles(&config.roles).await?;
    app.apply_organization_members(&config.organizations, &mut pending)
        .await?;
    app.apply_projects(&config.projects).await?;
    app.apply_integrations(&config.integrations).await?;
    app.apply_caches(&config.caches).await?;
    app.apply_api_keys(&config.api_keys).await?;
    app.apply_workers(&config.workers).await?;
    app.unmark_removed_entities(config, delete_state).await?;

    tracing::info!("State applied successfully");
    Ok(pending)
}

// ── Credential / lookup helpers ───────────────────────────────────────────────

fn credentials_dir() -> String {
    std::env::var("GRADIENT_CREDENTIALS_DIR")
        .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string())
}

/// Reads `${GRADIENT_CREDENTIALS_DIR}/gradient_${kind}_${name}_${suffix}` and
/// returns `(contents, path)`. The path is returned alongside so callers can
/// embed it in downstream validation errors.
fn read_credential(
    kind: &str,
    name: &str,
    suffix: &str,
    label: &str,
) -> Result<(String, String), DynError> {
    let path = format!(
        "{}/gradient_{}_{}_{}",
        credentials_dir(),
        kind,
        name,
        suffix
    );
    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {} {}: {}", label, path, e))?;
    Ok((contents, path))
}

fn lookup_id<T: Copy>(map: &HashMap<String, T>, name: &str, kind: &str) -> Result<T, DynError> {
    map.get(name)
        .copied()
        .ok_or_else(|| format!("{} '{}' not found", kind, name).into())
}

/// Loads the org's inbound integrations keyed by name.
///
/// `reporter_push` and `reporter_pull_request` triggers reference an inbound
/// integration by name. Auto-managed GitHub App rows are seeded once per org as
/// **two** integrations sharing `name = "github"` (one `Inbound`, one
/// `Outbound`), so a collect that ignores `kind` collapses them into a single
/// arbitrary entry - sometimes the outbound id, which makes the webhook
/// resolver's inbound lookup miss and the trigger never fires.
async fn inbound_integrations_by_name<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<HashMap<String, IntegrationId>, sea_orm::DbErr> {
    Ok(integration::Entity::find()
        .filter(integration::Column::Organization.eq(org_id))
        .filter(integration::Column::Kind.eq(i16::from(IntegrationKind::Inbound)))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.name, r.id))
        .collect())
}

async fn outbound_integrations_by_name<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<HashMap<String, IntegrationId>, sea_orm::DbErr> {
    Ok(integration::Entity::find()
        .filter(integration::Column::Organization.eq(org_id))
        .filter(integration::Column::Kind.eq(i16::from(IntegrationKind::Outbound)))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.name, r.id))
        .collect())
}

// ── managed_keep_sets ─────────────────────────────────────────────────────────

/// Names of state-managed rows that must remain `managed` after reconciliation.
///
/// Each set is built from the value's `name` (or `username`) field - the same
/// field every `apply_*` writes to the DB row. Using the attrset key here
/// instead would delete or unmanage rows whose Nix-side `name = "…"` was
/// overridden away from the attrset key (e.g. `projects.foo = { name = "main"; }`).
struct ManagedKeepSets<'a> {
    usernames: HashSet<&'a String>,
    org_names: HashSet<&'a String>,
    project_names: HashSet<&'a String>,
    cache_names: HashSet<&'a String>,
    api_key_names: HashSet<&'a String>,
}

fn managed_keep_sets(config: &StateConfiguration) -> ManagedKeepSets<'_> {
    ManagedKeepSets {
        usernames: config.users.values().map(|u| &u.username).collect(),
        org_names: config.organizations.values().map(|o| &o.name).collect(),
        project_names: config.projects.values().map(|p| &p.name).collect(),
        cache_names: config.caches.values().map(|c| &c.name).collect(),
        api_key_names: config.api_keys.values().map(|k| &k.name).collect(),
    }
}

// ── unmark_managed! macro ─────────────────────────────────────────────────────

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

// ── StateApplicator ───────────────────────────────────────────────────────────

/// Applies a [`StateConfiguration`] to the database.
///
/// Captures the database connection and crypt secret so each `apply_*` method
/// does not repeat those parameters.
struct StateApplicator<'a> {
    db: &'a DatabaseConnection,
    crypt_secret_file: &'a str,
    email_enabled: bool,
}

impl<'a> StateApplicator<'a> {
    // ── Lookup helpers ────────────────────────────────────────────────────────

    async fn user_lookup(&self) -> Result<HashMap<String, UserId>, DynError> {
        let users = user::Entity::find().all(self.db).await?;
        Ok(users.into_iter().map(|u| (u.username, u.id)).collect())
    }

    async fn org_lookup(&self) -> Result<HashMap<String, OrganizationId>, DynError> {
        let orgs = organization::Entity::find().all(self.db).await?;
        Ok(orgs.into_iter().map(|o| (o.name, o.id)).collect())
    }

    /// Encrypt `plain` with the configured crypt secret and return its
    /// base64-encoded form. `what` describes the secret for error messages.
    fn encrypt_to_b64(&self, plain: &str, what: &str) -> Result<String, DynError> {
        let secret = load_secret_bytes(self.crypt_secret_file)
            .map_err(|e| format!("Failed to load crypt secret: {}", e))?;
        let bytes = crypter::encrypt_with_password(secret.expose(), plain)
            .ok_or_else(|| format!("Failed to encrypt {}", what))?;
        Ok(general_purpose::STANDARD.encode(&bytes))
    }

    // ── apply_users ───────────────────────────────────────────────────────────

    async fn apply_users(&self, state_users: &HashMap<String, StateUser>) -> Result<(), DynError> {
        for state_user in state_users.values() {
            // When password_file is set in the state config, a matching
            // systemd credential is loaded under GRADIENT_CREDENTIALS_DIR.
            // When unset (OIDC-only user), we store `None` so the OIDC
            // login flow in `web::authorization::oidc` will accept the
            // account instead of rejecting with "already exists with
            // password authentication".
            let password_hash = if state_user.password_file.is_some() {
                let (contents, path) =
                    read_credential("user", &state_user.username, "password", "password file")?;
                Some(parse_password_phc(&contents, &path)?)
            } else {
                None
            };

            let existing_user = user::Entity::find()
                .filter(user::Column::Username.eq(&state_user.username))
                .one(self.db)
                .await?;

            let now = now();

            if let Some(existing) = existing_user {
                let mut user: user::ActiveModel = existing.into();
                user.name = Set(state_user.name.clone());
                user.email = Set(state_user.email.clone());
                user.password = Set(password_hash.clone());
                user.email_verified = Set(state_user.email_verified);
                user.superuser = Set(state_user.superuser);
                user.managed = Set(true);
                user.update(self.db).await?;
                tracing::info!(username = %state_user.username, "Updated managed user");
            } else {
                let user = user::ActiveModel {
                    id: Set(UserId::now_v7()),
                    username: Set(state_user.username.clone()),
                    name: Set(state_user.name.clone()),
                    email: Set(state_user.email.clone()),
                    password: Set(password_hash),
                    last_login_at: Set(now),
                    created_at: Set(now),
                    email_verified: Set(state_user.email_verified),
                    email_verification_token: Set(None),
                    email_verification_token_expires: Set(None),
                    managed: Set(true),
                    superuser: Set(state_user.superuser),
                    oidc_issuer: Set(None),
                    oidc_subject: Set(None),
                };
                user.insert(self.db).await?;
                tracing::info!(username = %state_user.username, "Created managed user");
            }
        }

        Ok(())
    }

    // ── apply_organizations_without_members ───────────────────────────────────

    /// Create/update the `organization` row (and seed the GitHub App
    /// integration if needed). Membership reconciliation happens later in
    /// `apply_organization_members`, after `apply_roles` so custom org roles
    /// referenced by `members` can be resolved against rows inserted in the
    /// same apply pass.
    async fn apply_organizations_without_members(
        &self,
        state_orgs: &HashMap<String, StateOrganization>,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;

        for state_org in state_orgs.values() {
            let (private_key, _) =
                read_credential("org", &state_org.name, "private_key", "private key file")?;
            let private_key = private_key.trim();

            let public_key = derive_public_key(private_key)?;
            let encrypted_private_key = self.encrypt_to_b64(private_key, "SSH private key")?;

            let created_by_id = lookup_id(&user_map, &state_org.created_by, "User")?;

            let existing_org = organization::Entity::find()
                .filter(organization::Column::Name.eq(&state_org.name))
                .one(self.db)
                .await?;

            let now = now();

            let org_id = if let Some(existing) = existing_org {
                let org_id = existing.id;
                let mut org: organization::ActiveModel = existing.into();
                org.display_name = Set(state_org.display_name.clone());
                org.description = Set(state_org.description.clone().unwrap_or_default());
                org.public_key = Set(public_key);
                org.private_key = Set(encrypted_private_key.clone());
                org.created_by = Set(created_by_id);
                org.public = Set(state_org.public);
                org.hide_build_requests = Set(state_org.hide_build_requests);
                // Only overwrite github_installation_id when state declares
                // it; otherwise leave the existing value (likely set by the
                // install webhook) intact.
                if let Some(id) = state_org.github_installation_id {
                    org.github_installation_id = Set(Some(id));
                }
                org.managed = Set(true);
                org.update(self.db).await?;
                tracing::info!(name = %state_org.name, "Updated managed organization");
                org_id
            } else {
                let org_id = OrganizationId::now_v7();
                let org = organization::ActiveModel {
                    id: Set(org_id),
                    name: Set(state_org.name.clone()),
                    display_name: Set(state_org.display_name.clone()),
                    description: Set(state_org.description.clone().unwrap_or_default()),
                    public_key: Set(public_key),
                    private_key: Set(encrypted_private_key),
                    public: Set(state_org.public),
                    hide_build_requests: Set(state_org.hide_build_requests),
                    created_by: Set(created_by_id),
                    created_at: Set(now),
                    managed: Set(true),
                    github_installation_id: Set(state_org.github_installation_id),
                };
                org.insert(self.db).await?;
                tracing::info!(name = %state_org.name, "Created managed organization");
                org_id
            };

            // Seed the auto-managed GitHub App integration rows whenever this
            // org has (or just acquired) an installation id. Idempotent.
            let installation_known = match organization::Entity::find_by_id(org_id)
                .one(self.db)
                .await?
            {
                Some(o) => o.github_installation_id.is_some(),
                None => false,
            };
            if installation_known {
                crate::ci::ensure_github_app_integrations(self.db, org_id, created_by_id).await?;
            }

        }

        Ok(())
    }

    // ── apply_organization_members ────────────────────────────────────────────

    /// Reconcile `organization_user` rows for every state-managed org.
    ///
    /// When `state_org.members` is empty, the legacy behavior applies:
    /// `created_by` is added as Admin if no row exists. When `members` is
    /// non-empty, the declared list is authoritative — see
    /// [`StateApplicator::apply_org_members`] for the per-org logic.
    async fn apply_organization_members(
        &self,
        state_orgs: &HashMap<String, StateOrganization>,
        pending: &mut PendingOrgMemberships,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;
        let org_map = self.org_lookup().await?;

        for state_org in state_orgs.values() {
            let org_id = lookup_id(&org_map, &state_org.name, "Organization")?;
            let created_by_id = lookup_id(&user_map, &state_org.created_by, "User")?;

            if state_org.members.is_empty() {
                let existing = organization_user::Entity::find()
                    .filter(organization_user::Column::Organization.eq(org_id))
                    .filter(organization_user::Column::User.eq(created_by_id))
                    .one(self.db)
                    .await?;

                if existing.is_none() {
                    organization_user::ActiveModel {
                        id: Set(OrganizationUserId::now_v7()),
                        organization: Set(org_id),
                        user: Set(created_by_id),
                        role: Set(BASE_ROLE_ADMIN_ID),
                    }
                    .insert(self.db)
                    .await?;
                    tracing::info!(
                        username = %state_org.created_by,
                        organization = %state_org.name,
                        "Added admin member to organization"
                    );
                }
            } else {
                self.apply_org_members(org_id, &state_org.name, &state_org.members, pending)
                    .await
                    .map_err(|e| {
                        format!(
                            "Failed to apply members for organization '{}': {}",
                            state_org.name, e
                        )
                    })?;
            }
        }

        Ok(())
    }

    /// Reconcile membership for a single state-managed organization whose
    /// `members` list is non-empty.
    ///
    /// - Missing users are recorded into `pending` and skipped (issue #94);
    ///   they'll be applied when the user later registers or signs in via
    ///   OIDC.
    /// - Built-in roles (`Admin`/`Write`/`View`) map to constant role IDs;
    ///   custom org roles resolve against `role` rows scoped to this org.
    /// - Drift: existing memberships not in the declared user set are
    ///   deleted. State owns the membership list when explicitly declared.
    async fn apply_org_members(
        &self,
        org_id: OrganizationId,
        org_name: &str,
        members: &[StateOrgMemberEntry],
        pending: &mut PendingOrgMemberships,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;

        let custom_roles: HashMap<String, RoleId> = role::Entity::find()
            .filter(role::Column::Organization.eq(org_id))
            .filter(role::Column::Managed.eq(true))
            .all(self.db)
            .await?
            .into_iter()
            .map(|r| (r.name, r.id))
            .collect();

        let mut declared_user_ids: HashSet<UserId> = HashSet::new();

        for member in members {
            let role_id = match member.role.as_str() {
                "Admin" => BASE_ROLE_ADMIN_ID,
                "Write" => BASE_ROLE_WRITE_ID,
                "View" => BASE_ROLE_VIEW_ID,
                name => *custom_roles.get(name).ok_or_else(|| -> DynError {
                    format!(
                        "Organization '{}' member '{}' references unknown role '{}'",
                        org_name, member.user, name
                    )
                    .into()
                })?,
            };

            match user_map.get(&member.user).copied() {
                Some(user_id) => {
                    declared_user_ids.insert(user_id);
                    let existing = organization_user::Entity::find()
                        .filter(organization_user::Column::Organization.eq(org_id))
                        .filter(organization_user::Column::User.eq(user_id))
                        .one(self.db)
                        .await?;
                    if let Some(row) = existing {
                        if row.role != role_id {
                            let mut active: organization_user::ActiveModel = row.into();
                            active.role = Set(role_id);
                            active.update(self.db).await?;
                            tracing::info!(
                                organization = %org_name,
                                user = %member.user,
                                "Updated organization membership role"
                            );
                        }
                    } else {
                        organization_user::ActiveModel {
                            id: Set(OrganizationUserId::now_v7()),
                            organization: Set(org_id),
                            user: Set(user_id),
                            role: Set(role_id),
                        }
                        .insert(self.db)
                        .await?;
                        tracing::info!(
                            organization = %org_name,
                            user = %member.user,
                            "Added organization member"
                        );
                    }
                }
                None => {
                    tracing::info!(
                        organization = %org_name,
                        user = %member.user,
                        "Declared member not yet registered; deferring until user creation"
                    );
                    pending
                        .entry(member.user.clone())
                        .or_default()
                        .push(PendingOrgMembership {
                            organization: org_id,
                            role: role_id,
                        });
                }
            }
        }

        let existing = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(org_id))
            .all(self.db)
            .await?;
        for row in existing {
            if !declared_user_ids.contains(&row.user) {
                let user_id = row.user;
                organization_user::Entity::delete_by_id(row.id)
                    .exec(self.db)
                    .await?;
                tracing::info!(
                    organization = %org_name,
                    %user_id,
                    "Removed organization member no longer in state"
                );
            }
        }

        Ok(())
    }

    // ── apply_projects ────────────────────────────────────────────────────────

    async fn apply_projects(
        &self,
        state_projects: &HashMap<String, StateProject>,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;
        let org_map = self.org_lookup().await?;

        for state_project in state_projects.values() {
            let created_by_id = lookup_id(&user_map, &state_project.created_by, "User")?;
            let org_id = lookup_id(&org_map, &state_project.organization, "Organization")?;

            let existing_project = project::Entity::find()
                .filter(project::Column::Name.eq(&state_project.name))
                .filter(project::Column::Organization.eq(org_id))
                .one(self.db)
                .await?;

            let now = now();

            let project_row = if let Some(existing) = existing_project {
                let project_id = existing.id;
                let mut proj: project::ActiveModel = existing.into();
                proj.organization = Set(org_id);
                proj.active = Set(state_project.active);
                proj.display_name = Set(state_project.display_name.clone());
                proj.description = Set(state_project.description.clone().unwrap_or_default());
                proj.repository = Set(state_project.repository.clone());
                proj.wildcard = Set(state_project.wildcard.clone());
                proj.keep_evaluations = Set(state_project.keep_evaluations);
                proj.created_by = Set(created_by_id);
                proj.concurrency = Set(i16::from(state_project.concurrency));
                proj.sign_cache = Set(state_project.sign_cache);
                proj.managed = Set(true);
                proj.update(self.db).await?;
                tracing::info!(name = %state_project.name, "Updated managed project");
                project::Entity::find_by_id(project_id)
                    .one(self.db)
                    .await?
                    .ok_or_else(|| {
                        format!("Project '{}' vanished after update", state_project.name)
                    })?
            } else {
                let proj = project::ActiveModel {
                    id: Set(ProjectId::now_v7()),
                    organization: Set(org_id),
                    name: Set(state_project.name.clone()),
                    active: Set(state_project.active),
                    display_name: Set(state_project.display_name.clone()),
                    description: Set(state_project.description.clone().unwrap_or_default()),
                    repository: Set(state_project.repository.clone()),
                    wildcard: Set(state_project.wildcard.clone()),
                    force_evaluation: Set(false),
                    created_by: Set(created_by_id),
                    last_evaluation: Set(None),
                    last_check_at: Set(now),
                    created_at: Set(now),
                    managed: Set(true),
                    keep_evaluations: Set(state_project.keep_evaluations),
                    concurrency: Set(i16::from(state_project.concurrency)),
                    sign_cache: Set(state_project.sign_cache),
                };
                let inserted = proj.insert(self.db).await?;
                tracing::info!(name = %state_project.name, "Created managed project");
                inserted
            };

            if let Some(triggers) = &state_project.triggers {
                let inbound_integrations_by_name =
                    inbound_integrations_by_name(self.db, org_id).await?;

                apply_project_triggers(
                    self.db,
                    &project_row,
                    triggers,
                    &inbound_integrations_by_name,
                )
                .await
                .map_err(|e| {
                    format!(
                        "Failed to apply triggers for project '{}': {}",
                        state_project.name, e
                    )
                })?;
            }

            self.apply_flake_input_overrides(project_row.id, &state_project.flake_input_overrides)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to apply flake input overrides for project '{}': {}",
                        state_project.name, e
                    )
                })?;

            self.apply_project_actions(
                project_row.id,
                created_by_id,
                org_id,
                &state_project.name,
                &state_project.actions,
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to apply actions for project '{}': {}",
                    state_project.name, e
                )
            })?;
        }

        Ok(())
    }

    // ── apply_project_actions ─────────────────────────────────────────────────

    async fn apply_project_actions(
        &self,
        project_id: ProjectId,
        created_by: UserId,
        org_id: OrganizationId,
        project_name: &str,
        desired: &[StateAction],
    ) -> Result<(), DynError> {
        let outbound = outbound_integrations_by_name(self.db, org_id).await?;
        let crypt_key = load_secret_bytes(self.crypt_secret_file)
            .map_err(|e| format!("Failed to load crypt secret: {}", e))?;

        let existing = EProjectAction::find()
            .filter(CProjectAction::Project.eq(project_id))
            .all(self.db)
            .await?;
        let existing_by_name: HashMap<String, MProjectAction> =
            existing.into_iter().map(|r| (r.name.clone(), r)).collect();

        let now = now();
        let mut declared: HashSet<String> = HashSet::new();

        for action in desired {
            if !declared.insert(action.name.clone()) {
                return Err(format!(
                    "duplicate action name '{}' in project '{}'",
                    action.name, project_name
                )
                .into());
            }

            let cfg = build_action_config(
                action,
                project_name,
                &outbound,
                self.email_enabled,
                crypt_key.expose(),
            )?;
            let cfg_json =
                serde_json::to_value(&cfg).map_err(|e| format!("encoding action config: {e}"))?;
            let events_json = serde_json::to_value(&action.events)
                .map_err(|e| format!("encoding action events: {e}"))?;
            let action_type_i16 = cfg.action_type().to_i16();

            match existing_by_name.get(&action.name) {
                Some(row) => {
                    let mut am: AProjectAction = row.clone().into();
                    am.action_type = Set(action_type_i16);
                    am.config = Set(cfg_json);
                    am.events = Set(events_json);
                    am.active = Set(action.active);
                    am.updated_at = Set(now);
                    am.update(self.db).await?;
                    tracing::info!(
                        project = %project_name,
                        action = %action.name,
                        "Updated project action"
                    );
                }
                None => {
                    let am = AProjectAction {
                        id: Set(ProjectActionId::now_v7()),
                        project: Set(project_id),
                        name: Set(action.name.clone()),
                        action_type: Set(action_type_i16),
                        config: Set(cfg_json),
                        events: Set(events_json),
                        active: Set(action.active),
                        last_fired_at: Set(None),
                        created_by: Set(created_by),
                        created_at: Set(now),
                        updated_at: Set(now),
                    };
                    am.insert(self.db).await?;
                    tracing::info!(
                        project = %project_name,
                        action = %action.name,
                        "Created project action"
                    );
                }
            }
        }

        for (name, row) in &existing_by_name {
            if declared.contains(name) {
                continue;
            }
            EProjectAction::delete_by_id(row.id).exec(self.db).await?;
            tracing::info!(
                project = %project_name,
                action = %name,
                "Deleted project action no longer declared in state"
            );
        }

        Ok(())
    }

    // ── apply_flake_input_overrides ───────────────────────────────────────────

    async fn apply_flake_input_overrides(
        &self,
        project_id: ProjectId,
        desired: &HashMap<String, StateFlakeInputOverride>,
    ) -> Result<(), DynError> {
        use entity::project_flake_input_override as pfio;

        for (name, o) in desired {
            if o.url.is_some() == o.keep_url {
                return Err(format!(
                    "flake input override '{name}' must set exactly one of `url` or `keep_url`",
                )
                .into());
            }
        }

        let existing = pfio::Entity::find()
            .filter(pfio::Column::Project.eq(project_id))
            .all(self.db)
            .await?;

        let existing_map: HashMap<String, pfio::Model> = existing
            .into_iter()
            .map(|r| (r.input_name.clone(), r))
            .collect();

        let now = chrono::Utc::now().naive_utc();

        for (name, o) in desired {
            let desired_url: Option<String> = if o.keep_url { None } else { o.url.clone() };
            match existing_map.get(name) {
                None => {
                    pfio::ActiveModel {
                        id: Set(entity::ids::FlakeInputOverrideId::now_v7()),
                        project: Set(project_id),
                        input_name: Set(name.clone()),
                        url: Set(desired_url),
                        created_at: Set(now),
                        updated_at: Set(now),
                    }
                    .insert(self.db)
                    .await?;
                }
                Some(row) if row.url != desired_url => {
                    let mut am: pfio::ActiveModel = row.clone().into();
                    am.url = Set(desired_url);
                    am.updated_at = Set(now);
                    am.update(self.db).await?;
                }
                Some(_) => {}
            }
        }

        for (name, row) in &existing_map {
            if !desired.contains_key(name) {
                let am: pfio::ActiveModel = row.clone().into();
                am.delete(self.db).await?;
            }
        }

        Ok(())
    }

    // ── apply_caches ──────────────────────────────────────────────────────────

    async fn apply_caches(
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
                let cache_model = cache::ActiveModel {
                    id: Set(cache_id),
                    name: Set(state_cache.name.clone()),
                    display_name: Set(state_cache.display_name.clone()),
                    description: Set(state_cache.description.clone().unwrap_or_default()),
                    active: Set(state_cache.active),
                    priority: Set(state_cache.priority),
                    local_priority: Set(state_cache.local_priority),
                    public_key: Set(public_key),
                    private_key: Set(encrypted_signing_key),
                    public: Set(state_cache.public),
                    created_by: Set(created_by_id),
                    created_at: Set(now),
                    managed: Set(true),
                };
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
                    let org_cache_model = organization_cache::ActiveModel {
                        id: Set(OrganizationCacheId::now_v7()),
                        organization: Set(org_id),
                        cache: Set(cache_id),
                        mode: Set(organization_cache::CacheSubscriptionMode::ReadWrite),
                    };
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
    async fn ensure_cache_creator_admin(
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
        let active = ACacheUser {
            id: Set(CacheUserId::now_v7()),
            cache: Set(cache_id),
            user: Set(created_by_id),
            role: Set(BASE_CACHE_ROLE_ADMIN_ID),
        };
        active.insert(self.db).await?;
        tracing::info!(
            cache = %cache_name,
            "Added cache creator as Admin (state-managed cache)"
        );
        Ok(())
    }

    async fn apply_cache_upstreams(
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
                    ACacheUpstream {
                        id: Set(CacheUpstreamId::now_v7()),
                        cache: Set(cache_id),
                        display_name: Set(name),
                        mode: Set(mode.clone()),
                        upstream_cache: Set(Some(upstream_id)),
                        url: Set(None),
                        public_key: Set(None),
                    }
                }
                StateUpstream::External {
                    display_name,
                    url,
                    public_key,
                } => ACacheUpstream {
                    id: Set(CacheUpstreamId::now_v7()),
                    cache: Set(cache_id),
                    display_name: Set(display_name.clone()),
                    mode: Set(CacheSubscriptionMode::ReadOnly),
                    upstream_cache: Set(None),
                    url: Set(Some(url.clone())),
                    public_key: Set(Some(public_key.clone())),
                },
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

    async fn apply_cache_roles_and_members(
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
                    crate::permissions::CachePermission::from_wire_name(wire).ok_or_else(|| {
                        format!(
                            "Cache '{}' role '{}' references unknown permission '{}'",
                            cache_name, entry.name, wire
                        )
                    })?;
                perms.push(p);
            }
            let mask = crate::permissions::cache_mask_from(&perms);

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
                let active = ACacheRole {
                    id: Set(RoleId::now_v7()),
                    name: Set(entry.name.clone()),
                    cache: Set(Some(cache_id)),
                    permission: Set(mask),
                    managed: Set(true),
                };
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
                let active = ACacheUser {
                    id: Set(CacheUserId::now_v7()),
                    cache: Set(cache_id),
                    user: Set(user_id),
                    role: Set(role_id),
                };
                active.insert(self.db).await?;
                tracing::info!(cache = %cache_name, user = %entry.user, "Added cache member");
            }

            declared_members.insert(user_id);
        }

        // (c) Drift reconciliation — remove managed roles not in declared set
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

    // ── apply_roles ───────────────────────────────────────────────────────────

    async fn apply_roles(&self, state_roles: &HashMap<String, StateRole>) -> Result<(), DynError> {
        let org_lookup = self.org_lookup().await?;

        for state_role in state_roles.values() {
            let org_id = lookup_id(&org_lookup, &state_role.organization, "Organization")?;

            let mut perms = Vec::with_capacity(state_role.permissions.len());
            for wire in &state_role.permissions {
                let p = crate::permissions::Permission::from_wire_name(wire).ok_or_else(|| {
                    format!(
                        "Role '{}' references unknown permission '{}'",
                        state_role.name, wire
                    )
                })?;
                perms.push(p);
            }
            if perms.is_empty() {
                return Err(format!(
                    "Role '{}' must declare at least one permission",
                    state_role.name
                )
                .into());
            }
            let mask = crate::permissions::mask_from(&perms);

            let existing = role::Entity::find()
                .filter(role::Column::Name.eq(&state_role.name))
                .filter(role::Column::Organization.eq(org_id))
                .one(self.db)
                .await?;

            if let Some(existing) = existing {
                let mut active: role::ActiveModel = existing.into();
                active.permission = Set(mask);
                active.managed = Set(true);
                active.update(self.db).await?;
                tracing::info!(name = %state_role.name, "Updated managed role");
            } else {
                let active = role::ActiveModel {
                    id: Set(RoleId::now_v7()),
                    name: Set(state_role.name.clone()),
                    organization: Set(Some(org_id)),
                    permission: Set(mask),
                    managed: Set(true),
                };
                active.insert(self.db).await?;
                tracing::info!(name = %state_role.name, "Created managed role");
            }
        }

        Ok(())
    }

    // ── apply_api_keys ────────────────────────────────────────────────────────

    async fn apply_api_keys(
        &self,
        state_api_keys: &HashMap<String, StateApiKey>,
    ) -> Result<(), DynError> {
        let user_lookup = self.user_lookup().await?;
        let org_lookup = self.org_lookup().await?;
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

            let mut perms = Vec::with_capacity(state_api_key.permissions.len());
            for wire in &state_api_key.permissions {
                let p = crate::permissions::Permission::from_wire_name(wire).ok_or_else(|| {
                    format!(
                        "API key '{}' references unknown permission '{}'",
                        state_api_key.name, wire
                    )
                })?;
                perms.push(p);
            }
            if perms.is_empty() {
                return Err(format!(
                    "API key '{}' must declare at least one permission",
                    state_api_key.name
                )
                .into());
            }
            let mask = crate::permissions::mask_from(&perms);

            let pinned_org = match &state_api_key.organization {
                None => None,
                Some(name) => Some(org_lookup.get(name).copied().ok_or_else(|| {
                    format!(
                        "Organization '{}' referenced by API key '{}' not found",
                        name, state_api_key.name
                    )
                })?),
            };

            let (key_value, key_path) =
                read_credential("api", &state_api_key.name, "key", "API key file")?;
            let key_hash = parse_api_key_hash(&key_value, &key_path)?;

            let existing_api_key = api::Entity::find()
                .filter(api::Column::Name.eq(&state_api_key.name))
                .filter(api::Column::OwnedBy.eq(owned_by_id))
                .one(self.db)
                .await?;

            if let Some(api_key_model) = existing_api_key {
                let mut api_key: api::ActiveModel = api_key_model.into();
                api_key.key = Set(key_hash);
                api_key.managed = Set(true);
                api_key.permission = Set(mask);
                api_key.organization = Set(pinned_org);
                api_key.update(self.db).await?;
                tracing::info!(name = %state_api_key.name, "Updated managed API key");
            } else {
                let api_key_model = api::ActiveModel {
                    id: Set(ApiId::now_v7()),
                    owned_by: Set(owned_by_id),
                    name: Set(state_api_key.name.clone()),
                    key: Set(key_hash),
                    last_used_at: Set(now),
                    created_at: Set(now),
                    managed: Set(true),
                    expires_at: Set(None),
                    revoked_at: Set(None),
                    permission: Set(mask),
                    organization: Set(pinned_org),
                    cache: Set(None),
                };
                api_key_model.insert(self.db).await?;
                tracing::info!(name = %state_api_key.name, "Created managed API key");
            }
        }

        Ok(())
    }

    // ── apply_workers ─────────────────────────────────────────────────────────

    async fn apply_workers(
        &self,
        state_workers: &HashMap<String, StateWorker>,
    ) -> Result<(), DynError> {
        let org_map = self.org_lookup().await?;
        let user_map = self.user_lookup().await?;

        for state_worker in state_workers.values() {
            let (token, _) = read_credential(
                "worker",
                &state_worker.worker_id,
                "token",
                "worker token file",
            )?;
            let token_hash = password_auth::generate_hash(token.trim());
            let created_by_id = lookup_id(&user_map, &state_worker.created_by, "User")?;

            let url = state_worker
                .url
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            for org_name in &state_worker.organizations {
                let peer_id = lookup_id(&org_map, org_name, "Organization")?;

                let existing = worker_registration::Entity::find()
                    .filter(worker_registration::Column::PeerId.eq(peer_id))
                    .filter(worker_registration::Column::WorkerId.eq(&state_worker.worker_id))
                    .one(self.db)
                    .await?;

                if let Some(existing) = existing {
                    let mut reg: worker_registration::ActiveModel = existing.into();
                    reg.token_hash = Set(token_hash.clone());
                    reg.managed = Set(true);
                    reg.url = Set(url.clone());
                    reg.display_name = Set(state_worker.display_name.clone());
                    reg.enable_fetch = Set(state_worker.enable_fetch);
                    reg.enable_eval = Set(state_worker.enable_eval);
                    reg.enable_build = Set(state_worker.enable_build);
                    reg.created_by = Set(Some(created_by_id));
                    reg.update(self.db).await?;
                    tracing::info!(
                        worker_id = %state_worker.worker_id,
                        organization = %org_name,
                        "Updated worker registration"
                    );
                } else {
                    let reg = worker_registration::ActiveModel {
                        id: Set(WorkerRegistrationId::now_v7()),
                        peer_id: Set(peer_id),
                        worker_id: Set(state_worker.worker_id.clone()),
                        token_hash: Set(token_hash.clone()),
                        managed: Set(true),
                        url: Set(url.clone()),
                        display_name: Set(state_worker.display_name.clone()),
                        active: Set(true),
                        enable_fetch: Set(state_worker.enable_fetch),
                        enable_eval: Set(state_worker.enable_eval),
                        enable_build: Set(state_worker.enable_build),
                        created_by: Set(Some(created_by_id)),
                        created_at: Set(now()),
                    };
                    reg.insert(self.db).await?;
                    tracing::info!(
                        worker_id = %state_worker.worker_id,
                        organization = %org_name,
                        "Created worker registration"
                    );
                }
            }
        }

        Ok(())
    }

    // ── apply_integrations ────────────────────────────────────────────────────

    async fn apply_integrations(
        &self,
        state_integrations: &HashMap<String, StateIntegration>,
    ) -> Result<(), DynError> {
        if state_integrations.is_empty() {
            return Ok(());
        }

        let org_map = self.org_lookup().await?;
        let user_map = self.user_lookup().await?;

        for state_int in state_integrations.values() {
            let org_id = org_map
                .get(&state_int.organization)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "Integration '{}' references unknown organization '{}'",
                        state_int.name, state_int.organization
                    )
                })?;

            let created_by_id = lookup_id(&user_map, &state_int.created_by, "User")?;

            let kind = match state_int.kind.as_str() {
                "inbound" => IntegrationKind::Inbound,
                "outbound" => IntegrationKind::Outbound,
                other => {
                    return Err(format!(
                        "Integration '{}' has invalid kind '{}': expected 'inbound' or 'outbound'",
                        state_int.name, other
                    )
                    .into());
                }
            };

            let forge = ForgeType::from_path_segment(&state_int.forge_type).ok_or_else(|| {
                format!(
                    "Integration '{}' has invalid forge_type '{}': expected gitea/forgejo/gitlab",
                    state_int.name, state_int.forge_type
                )
            })?;
            if matches!(forge, ForgeType::GitHub) {
                return Err(format!(
                    "Integration '{}' has forge_type 'github': GitHub integrations are managed \
                     through the server-wide GitHub App; bind installations on the org via \
                     `github_installation_id`, not via integration rows.",
                    state_int.name
                )
                .into());
            }
            if state_int.name == GITHUB_APP_INTEGRATION_NAME {
                return Err(format!(
                    "Integration '{}' uses the reserved name '{}' (auto-managed GitHub App row).",
                    state_int.name, GITHUB_APP_INTEGRATION_NAME
                )
                .into());
            }

            let encrypted_secret = self.read_and_encrypt_integration_field(
                state_int.secret_file.as_deref(),
                &state_int.name,
                "secret",
            )?;
            let encrypted_token = self.read_and_encrypt_integration_field(
                state_int.access_token_file.as_deref(),
                &state_int.name,
                "token",
            )?;

            let endpoint = state_int
                .endpoint_url
                .as_deref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let existing = integration::Entity::find()
                .filter(integration::Column::Organization.eq(org_id))
                .filter(integration::Column::Kind.eq(i16::from(kind)))
                .filter(integration::Column::Name.eq(&state_int.name))
                .one(self.db)
                .await?;

            let display_name = state_int
                .display_name
                .clone()
                .unwrap_or_else(|| state_int.name.clone());

            if let Some(existing) = existing {
                let mut active: integration::ActiveModel = existing.into();
                active.display_name = Set(display_name);
                active.forge_type = Set(i16::from(forge));
                active.endpoint_url = Set(endpoint);
                active.secret = Set(encrypted_secret);
                active.access_token = Set(encrypted_token);
                active.created_by = Set(created_by_id);
                active.update(self.db).await?;
                tracing::info!(name = %state_int.name, "Updated managed integration");
            } else {
                let row = integration::ActiveModel {
                    id: Set(IntegrationId::now_v7()),
                    organization: Set(org_id),
                    name: Set(state_int.name.clone()),
                    display_name: Set(display_name),
                    kind: Set(i16::from(kind)),
                    forge_type: Set(i16::from(forge)),
                    secret: Set(encrypted_secret),
                    endpoint_url: Set(endpoint),
                    access_token: Set(encrypted_token),
                    created_by: Set(created_by_id),
                    created_at: Set(now()),
                };
                row.insert(self.db).await?;
                tracing::info!(name = %state_int.name, "Created managed integration");
            }
        }

        Ok(())
    }

    /// Read `${creds}/gradient_integration_${name}_${suffix}` and encrypt its
    /// trimmed contents with the webhook secret. Returns `Ok(None)` when the
    /// state config did not declare a credential file (`field_set` is `None`).
    fn read_and_encrypt_integration_field(
        &self,
        field_set: Option<&str>,
        int_name: &str,
        suffix: &str,
    ) -> Result<Option<String>, DynError> {
        if field_set.is_none() {
            return Ok(None);
        }
        let label = format!("integration {} file", suffix);
        let (plain, _) = read_credential("integration", int_name, suffix, &label)?;
        let encrypted =
            encrypt_secret_with_file(self.crypt_secret_file, plain.trim()).map_err(|e| {
                format!(
                    "Failed to encrypt {} for integration '{}': {}",
                    suffix, int_name, e
                )
            })?;
        Ok(Some(encrypted))
    }

    // ── unmark_removed_entities ───────────────────────────────────────────────

    async fn unmark_removed_entities(
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

        Ok(())
    }
}

// ── Pending-membership backfill ──────────────────────────────────────────────

/// Apply any pending state-managed org memberships for `username` against
/// `user_id`. Idempotent: existing rows are updated to the declared role,
/// missing rows are inserted. Returns the number of memberships applied
/// (`Ok(0)` when the username has no pending entries).
///
/// Called from the user-creation paths (`POST /user` and OIDC first-login)
/// so a member declared in state for a not-yet-registered user becomes
/// effective the instant that user joins.
pub async fn apply_pending_org_memberships<C: ConnectionTrait>(
    db: &C,
    pending: &PendingOrgMemberships,
    username: &str,
    user_id: UserId,
) -> Result<usize, sea_orm::DbErr> {
    let Some(entries) = pending.get(username) else {
        return Ok(0);
    };
    let mut applied = 0usize;
    for entry in entries {
        let existing = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(entry.organization))
            .filter(organization_user::Column::User.eq(user_id))
            .one(db)
            .await?;
        match existing {
            Some(row) if row.role == entry.role => {}
            Some(row) => {
                let mut active: organization_user::ActiveModel = row.into();
                active.role = Set(entry.role);
                active.update(db).await?;
                applied += 1;
            }
            None => {
                organization_user::ActiveModel {
                    id: Set(OrganizationUserId::now_v7()),
                    organization: Set(entry.organization),
                    user: Set(user_id),
                    role: Set(entry.role),
                }
                .insert(db)
                .await?;
                applied += 1;
            }
        }
    }
    if applied > 0 {
        tracing::info!(
            username,
            count = applied,
            "Applied pending state-managed org memberships for newly-registered user"
        );
    }
    Ok(applied)
}

// ── Trigger sync ─────────────────────────────────────────────────────────────

async fn apply_project_triggers<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    desired: &[StateTrigger],
    integrations_by_name: &HashMap<String, IntegrationId>,
) -> anyhow::Result<()> {
    if desired.is_empty() {
        anyhow::bail!("project '{}' must have at least one trigger", project.name);
    }

    let mut desired_by_key: HashMap<String, (TriggerConfig, bool)> = HashMap::new();
    for t in desired {
        let cfg = build_trigger_config(t, integrations_by_name)?;
        let key = trigger_key(&cfg);
        desired_by_key.insert(key, (cfg, t.active));
    }

    let existing: Vec<MProjectTrigger> = EProjectTrigger::find()
        .filter(CProjectTrigger::Project.eq(project.id))
        .all(db)
        .await?;

    let mut existing_by_key: HashMap<String, MProjectTrigger> = HashMap::new();
    for row in existing {
        let cfg = TriggerConfig::parse_row(row.trigger_type, &row.config)
            .context("parse existing trigger")?;
        let key = trigger_key(&cfg);
        existing_by_key.insert(key, row);
    }

    let now = crate::types::now();

    for (key, (cfg, active)) in &desired_by_key {
        if existing_by_key.contains_key(key) {
            continue;
        }
        AProjectTrigger {
            id: Set(ProjectTriggerId::now_v7()),
            project: Set(project.id),
            trigger_type: Set(i16::from(cfg.trigger_type())),
            config: Set(cfg.to_db_json()),
            active: Set(*active),
            last_fired_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
    }

    for (key, row) in existing_by_key {
        if let Some((_, active)) = desired_by_key.get(&key) {
            if row.active != *active {
                let mut a: AProjectTrigger = row.into();
                a.active = Set(*active);
                a.updated_at = Set(now);
                a.update(db).await?;
            }
        } else {
            EProjectTrigger::delete_by_id(row.id).exec(db).await?;
        }
    }

    Ok(())
}

fn build_trigger_config(
    t: &StateTrigger,
    integrations: &HashMap<String, IntegrationId>,
) -> anyhow::Result<TriggerConfig> {
    use crate::types::triggers::TriggerType as TT;
    let cfg = match t.trigger_type {
        TT::Polling => {
            let interval = t
                .config
                .get("interval_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(300) as u32;
            TriggerConfig::Polling {
                interval_secs: interval,
                branch: t
                    .config
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned()),
            }
        }
        TT::ReporterPush | TT::ReporterPullRequest => {
            let name = t
                .integration
                .as_ref()
                .context("reporter trigger requires `integration` name")?;
            let id = *integrations
                .get(name)
                .with_context(|| format!("unknown integration: {name}"))?;
            if t.trigger_type == TT::ReporterPush {
                TriggerConfig::ReporterPush {
                    integration_id: id,
                    branches: t
                        .config
                        .get("branches")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    tags: t
                        .config
                        .get("tags")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    releases_only: t
                        .config
                        .get("releases_only")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                }
            } else {
                TriggerConfig::ReporterPullRequest {
                    integration_id: id,
                    branches: t
                        .config
                        .get("branches")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    actions: t
                        .config
                        .get("actions")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_else(|| {
                            vec!["opened".into(), "synchronize".into(), "reopened".into()]
                        }),
                    require_approval: t
                        .config
                        .get("require_approval")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                }
            }
        }
        TT::Time => {
            let cron = t
                .config
                .get("cron")
                .and_then(|v| v.as_str())
                .context("time trigger requires `cron`")?
                .to_string();
            TriggerConfig::Time { cron }
        }
    };
    cfg.validate().context("trigger config validation failed")?;
    Ok(cfg)
}

fn trigger_key(cfg: &TriggerConfig) -> String {
    let json = cfg.to_db_json();
    let canonical = serde_json::to_string(&json).unwrap_or_default();
    format!("{}|{}", i16::from(cfg.trigger_type()), canonical)
}

// ── Action sync ──────────────────────────────────────────────────────────────

/// Build a stored `ActionConfig` from a declared `StateAction`. Tokens for
/// `send_web_request` are loaded from the systemd credential file
/// `gradient_action_${name}_token` and encrypted with the server's crypt key
/// before storage, matching the REST `create_action` path.
fn build_action_config(
    a: &StateAction,
    project_name: &str,
    outbound: &HashMap<String, IntegrationId>,
    email_enabled: bool,
    crypt_key: &[u8],
) -> Result<ActionConfig, DynError> {
    let want = |k: &str| -> Result<&serde_json::Value, DynError> {
        a.config
            .get(k)
            .ok_or_else(|| format!("action '{}' config missing '{}'", a.name, k).into())
    };

    match a.action_type.as_str() {
        "send_mail" => {
            if !email_enabled {
                return Err(format!(
                    "action '{}' in project '{}' is type 'send_mail' but SMTP is not enabled on this server",
                    a.name, project_name
                )
                .into());
            }
            let recipients: Vec<String> = want("recipients")?
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .ok_or_else(|| format!("action '{}': recipients must be a list", a.name))?;
            if recipients.is_empty() {
                return Err(format!("action '{}': recipients must be non-empty", a.name).into());
            }
            let subject_template = a
                .config
                .get("subject_template")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            Ok(ActionConfig::SendMail {
                recipients,
                subject_template,
            })
        }
        "send_web_request" => {
            let url = want("url")?
                .as_str()
                .ok_or_else(|| format!("action '{}': url must be a string", a.name))?
                .to_owned();
            crate::ci::http_validation::validate_webhook_url(&url)
                .map_err(|e| format!("action '{}': {}", a.name, e))?;
            let token = if a.config.get("token_file").is_some() {
                let (plain, _) =
                    read_credential("action", &a.name, "token", "action token file")?;
                let plain = plain.trim();
                let enc = crate::ci::actions::encrypt_action_secret(plain, crypt_key)
                    .map_err(|e| format!("encrypt action token: {e}"))?;
                Some(enc)
            } else {
                None
            };
            Ok(ActionConfig::SendWebRequest { url, token })
        }
        "forge_status_report" => {
            if !a.events.is_empty() {
                return Err(format!(
                    "action '{}': forge_status_report cannot carry custom events",
                    a.name
                )
                .into());
            }
            let int_name = want("integration")?
                .as_str()
                .ok_or_else(|| format!("action '{}': integration must be a string", a.name))?;
            let integration_id = *outbound.get(int_name).ok_or_else(|| {
                format!(
                    "action '{}': outbound integration '{}' not found in project's organization",
                    a.name, int_name
                )
            })?;
            Ok(ActionConfig::ForgeStatusReport { integration_id })
        }
        other => Err(format!(
            "action '{}' has invalid type '{}': expected send_mail/send_web_request/forge_status_report",
            a.name, other
        )
        .into()),
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Validate the contents of a user password credential file. The file must
/// contain an argon2 PHC hash (e.g. produced by `gradient-server hash` or the
/// `argon2 -id -e` CLI). The plaintext password is never stored - the server
/// only accepts the pre-hashed PHC string and passes it through to the DB.
fn parse_password_phc(contents: &str, path: &str) -> Result<String, DynError> {
    let phc = contents.trim().to_string();
    if !phc.starts_with("$argon2") {
        return Err(format!(
            "Password file {} does not contain an argon2 PHC hash (expected to start with `$argon2`). \
             Generate one with `gradient hash` or `argon2 ... -id -e`.",
            path
        )
        .into());
    }
    Ok(phc)
}

fn parse_api_key_hash(contents: &str, path: &str) -> Result<String, DynError> {
    let v = contents.trim();
    if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "API key file {} must contain a lowercase 64-char hex SHA-256 hash of the token \
             (e.g. `printf %s \"$TOKEN\" | sha256sum | cut -d' ' -f1`).",
            path
        )
        .into());
    }
    Ok(v.to_ascii_lowercase())
}

fn derive_public_key(private_key: &str) -> Result<String> {
    let private_key =
        PrivateKey::from_openssh(private_key).context("Failed to parse private key")?;

    let public_key = private_key
        .public_key()
        .to_openssh()
        .context("Failed to derive public key")?;

    let key_parts: Vec<&str> = public_key.split_whitespace().collect();
    let cleaned_key = if key_parts.len() >= 2 {
        format!("{} {}", key_parts[0], key_parts[1])
    } else {
        public_key.to_string()
    };

    Ok(cleaned_key)
}

#[cfg(test)]
mod password_phc_tests {
    use super::parse_password_phc;

    #[test]
    fn accepts_argon2id_phc_hash() {
        let h = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let parsed = parse_password_phc(h, "/tmp/p").unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn trims_trailing_whitespace_and_newlines() {
        let h = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$dGVzdA";
        let with_ws = format!("{h}\n  \n");
        let parsed = parse_password_phc(&with_ws, "/tmp/p").unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn rejects_plaintext_password() {
        let err = parse_password_phc("hunter2\n", "/tmp/p").unwrap_err();
        assert!(err.to_string().contains("argon2 PHC hash"));
    }

    #[test]
    fn rejects_other_phc_algorithms() {
        let h = "$pbkdf2-sha256$i=600000$c2FsdA$aGFzaA";
        let err = parse_password_phc(h, "/tmp/p").unwrap_err();
        assert!(err.to_string().contains("argon2"));
    }
}

#[cfg(test)]
mod api_key_hash_tests {
    use super::parse_api_key_hash;

    const VALID: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn accepts_64_char_hex() {
        assert_eq!(parse_api_key_hash(VALID, "/tmp/k").unwrap(), VALID);
    }

    #[test]
    fn trims_trailing_whitespace() {
        let with_ws = format!("{VALID}\n");
        assert_eq!(parse_api_key_hash(&with_ws, "/tmp/k").unwrap(), VALID);
    }

    #[test]
    fn lowercases_uppercase_hex() {
        let upper = VALID.to_ascii_uppercase();
        assert_eq!(parse_api_key_hash(&upper, "/tmp/k").unwrap(), VALID);
    }

    #[test]
    fn rejects_plaintext_token() {
        let err = parse_api_key_hash("notahashbutaverylongstring", "/tmp/k").unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }

    #[test]
    fn rejects_short_hex() {
        let err = parse_api_key_hash("deadbeef", "/tmp/k").unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }

    #[test]
    fn rejects_non_hex_chars() {
        let bad = "z".repeat(64);
        let err = parse_api_key_hash(&bad, "/tmp/k").unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }
}

#[cfg(test)]
mod helper_tests {
    use super::{credentials_dir, lookup_id, read_credential};
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn lookup_id_returns_id_when_present() {
        let id = Uuid::now_v7();
        let mut m = HashMap::new();
        m.insert("alice".to_string(), id);
        assert_eq!(lookup_id(&m, "alice", "User").unwrap(), id);
    }

    #[test]
    fn lookup_id_errors_with_kind_and_name() {
        let m: HashMap<String, Uuid> = HashMap::new();
        let err = lookup_id(&m, "ghost", "User").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("User"));
        assert!(s.contains("ghost"));
    }

    #[test]
    fn read_credential_default_dir_when_env_unset() {
        // Without GRADIENT_CREDENTIALS_DIR set, credentials_dir() returns the
        // built-in systemd-credentials path. The read fails (no such file),
        // so we just verify the error embeds the expected suffix and label.
        // We don't assert on the env var (other tests run in parallel and
        // may set it concurrently).
        let err = read_credential("user", "alice", "password", "password file").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("password file"));
        assert!(s.contains("gradient_user_alice_password"));
    }

    #[test]
    fn credentials_dir_returns_nonempty() {
        // We can't assert the exact value without racing on env state, but it
        // must always be a non-empty path so format!() composes a valid path.
        assert!(!credentials_dir().is_empty());
    }
}

#[cfg(test)]
mod trigger_helper_tests {
    use super::{build_trigger_config, trigger_key};
    use crate::state::StateTrigger;
    use crate::types::IntegrationId;
    use crate::types::triggers::{TriggerConfig, TriggerType};
    use std::collections::HashMap;

    fn polling_trigger(interval_secs: u64) -> StateTrigger {
        StateTrigger {
            trigger_type: TriggerType::Polling,
            integration: None,
            config: serde_json::json!({ "interval_secs": interval_secs }),
            active: true,
        }
    }

    fn empty_integrations() -> HashMap<String, IntegrationId> {
        HashMap::new()
    }

    #[test]
    fn build_polling_trigger() {
        let t = polling_trigger(60);
        let cfg = build_trigger_config(&t, &empty_integrations()).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Polling {
                interval_secs: 60,
                branch: None
            }
        );
    }

    #[test]
    fn build_polling_defaults_interval_when_missing() {
        let t = StateTrigger {
            trigger_type: TriggerType::Polling,
            integration: None,
            config: serde_json::Value::Null,
            active: true,
        };
        let cfg = build_trigger_config(&t, &empty_integrations()).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Polling {
                interval_secs: 300,
                branch: None
            }
        );
    }

    #[test]
    fn build_polling_rejects_too_small_interval() {
        let t = polling_trigger(5);
        let err = build_trigger_config(&t, &empty_integrations()).unwrap_err();
        let full = format!("{err:#}");
        assert!(
            full.contains("interval_secs") || full.contains("validation"),
            "expected polling interval rejection, got: {full}"
        );
    }

    #[test]
    fn build_time_trigger() {
        let t = StateTrigger {
            trigger_type: TriggerType::Time,
            integration: None,
            config: serde_json::json!({ "cron": "0 0 2 * * *" }),
            active: true,
        };
        let cfg = build_trigger_config(&t, &empty_integrations()).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Time {
                cron: "0 0 2 * * *".into()
            }
        );
    }

    #[test]
    fn build_time_trigger_requires_cron() {
        let t = StateTrigger {
            trigger_type: TriggerType::Time,
            integration: None,
            config: serde_json::json!({}),
            active: true,
        };
        let err = build_trigger_config(&t, &empty_integrations()).unwrap_err();
        assert!(err.to_string().contains("cron"));
    }

    #[test]
    fn build_reporter_push_requires_integration_name() {
        let t = StateTrigger {
            trigger_type: TriggerType::ReporterPush,
            integration: None,
            config: serde_json::json!({}),
            active: true,
        };
        let err = build_trigger_config(&t, &empty_integrations()).unwrap_err();
        assert!(err.to_string().contains("integration"));
    }

    #[test]
    fn build_reporter_push_errors_on_unknown_integration() {
        let t = StateTrigger {
            trigger_type: TriggerType::ReporterPush,
            integration: Some("github-app".into()),
            config: serde_json::json!({}),
            active: true,
        };
        let err = build_trigger_config(&t, &empty_integrations()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("github-app"),
            "expected integration name in error: {msg}"
        );
    }

    #[test]
    fn build_reporter_push_with_known_integration() {
        let int_id = IntegrationId::nil();
        let mut integrations = HashMap::new();
        integrations.insert("gh".into(), int_id);

        let t = StateTrigger {
            trigger_type: TriggerType::ReporterPush,
            integration: Some("gh".into()),
            config: serde_json::json!({ "branches": ["main"], "tags": [], "releases_only": false }),
            active: true,
        };
        let cfg = build_trigger_config(&t, &integrations).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::ReporterPush {
                integration_id: int_id,
                branches: vec!["main".into()],
                tags: vec![],
                releases_only: false,
            }
        );
    }

    #[test]
    fn trigger_key_differs_by_type() {
        let polling = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
        };
        let time = TriggerConfig::Time {
            cron: "0 0 * * * *".into(),
        };
        assert_ne!(trigger_key(&polling), trigger_key(&time));
    }

    #[test]
    fn trigger_key_stable_for_same_config() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 300,
            branch: None,
        };
        assert_eq!(trigger_key(&cfg), trigger_key(&cfg));
    }

    #[test]
    fn state_trigger_serde_round_trip() {
        let json = serde_json::json!({
            "type": "polling",
            "config": { "interval_secs": 120 },
            "active": true
        });
        let t: StateTrigger = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(t.trigger_type, TriggerType::Polling);
        assert!(t.active);
    }

    #[test]
    fn state_trigger_active_defaults_to_true() {
        let json = serde_json::json!({
            "type": "polling",
            "config": { "interval_secs": 60 }
        });
        let t: StateTrigger = serde_json::from_value(json).unwrap();
        assert!(t.active);
    }

    // TODO: integration test for apply_project_triggers full DB round-trip (T30 smoke)
}

#[cfg(test)]
mod inbound_integration_lookup_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org_id() -> OrganizationId {
        OrganizationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
    }

    /// Regression: the SELECT behind `inbound_integrations_by_name` must
    /// restrict to `kind = Inbound`. The auto-managed GitHub App seeds two
    /// rows per org sharing `name = "github"` (one inbound, one outbound). A
    /// query that ignores `kind` collapses them in the resulting HashMap and
    /// silently stores the outbound id on `reporter_push`/`reporter_pull_request`
    /// triggers, so the webhook resolver's inbound lookup never matches.
    #[tokio::test]
    async fn inbound_integrations_lookup_sql_filters_kind_inbound() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<integration::Model>::new()])
            .into_connection();

        inbound_integrations_by_name(&db, org_id()).await.unwrap();

        let logs = db.into_transaction_log();
        let select = logs
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| s.sql.to_lowercase().contains("from \"integration\""))
            .expect("expected a SELECT FROM integration statement");

        assert!(
            select.sql.contains("\"kind\""),
            "SELECT must filter by kind column: {}",
            select.sql,
        );

        let inbound = i16::from(IntegrationKind::Inbound);
        let bound: Vec<sea_orm::Value> = select
            .values
            .as_ref()
            .map(|v| v.0.clone())
            .unwrap_or_default();
        assert!(
            bound
                .iter()
                .any(|v| matches!(v, sea_orm::Value::SmallInt(Some(n)) if *n == inbound)),
            "SELECT must bind inbound kind as SmallInt({inbound}); bound values: {bound:?}",
        );
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

#[cfg(test)]
mod pending_membership_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn apply_pending_returns_zero_for_unknown_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let pending: PendingOrgMemberships = HashMap::new();
        let count = apply_pending_org_memberships(&db, &pending, "ghost", UserId::now_v7())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[cfg(test)]
mod action_helper_tests {
    use super::*;
    use uuid::Uuid;

    fn key() -> Vec<u8> {
        b"01234567890123456789012345678901".to_vec()
    }

    #[test]
    fn build_send_mail_round_trip() {
        let a = StateAction {
            name: "ops".into(),
            action_type: "send_mail".into(),
            active: true,
            events: vec!["build.failed".into()],
            config: serde_json::json!({
                "recipients": ["ops@example.com"],
                "subject_template": "[Gradient] {event}",
            }),
        };
        let cfg = build_action_config(&a, "web", &HashMap::new(), true, &key()).unwrap();
        match cfg {
            ActionConfig::SendMail {
                recipients,
                subject_template,
            } => {
                assert_eq!(recipients, vec!["ops@example.com".to_string()]);
                assert_eq!(subject_template.as_deref(), Some("[Gradient] {event}"));
            }
            other => panic!("expected SendMail, got {other:?}"),
        }
    }

    #[test]
    fn build_send_mail_rejects_when_email_disabled() {
        let a = StateAction {
            name: "ops".into(),
            action_type: "send_mail".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "recipients": ["ops@example.com"] }),
        };
        let err = build_action_config(&a, "web", &HashMap::new(), false, &key()).unwrap_err();
        assert!(err.to_string().contains("SMTP"), "got: {err}");
    }

    #[test]
    fn build_send_mail_requires_non_empty_recipients() {
        let a = StateAction {
            name: "ops".into(),
            action_type: "send_mail".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "recipients": [] }),
        };
        let err = build_action_config(&a, "web", &HashMap::new(), true, &key()).unwrap_err();
        assert!(err.to_string().contains("recipients"), "got: {err}");
    }

    #[test]
    fn build_send_web_request_without_token() {
        let a = StateAction {
            name: "hook".into(),
            action_type: "send_web_request".into(),
            active: true,
            events: vec!["build.completed".into()],
            config: serde_json::json!({ "url": "https://hooks.example.com/x" }),
        };
        let cfg = build_action_config(&a, "web", &HashMap::new(), true, &key()).unwrap();
        match cfg {
            ActionConfig::SendWebRequest { url, token } => {
                assert_eq!(url, "https://hooks.example.com/x");
                assert!(token.is_none());
            }
            other => panic!("expected SendWebRequest, got {other:?}"),
        }
    }

    #[test]
    fn build_forge_status_report_resolves_integration() {
        let int_id = IntegrationId::new(Uuid::nil());
        let mut outbound = HashMap::new();
        outbound.insert("gitea-prod".to_string(), int_id);
        let a = StateAction {
            name: "status".into(),
            action_type: "forge_status_report".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "integration": "gitea-prod" }),
        };
        let cfg = build_action_config(&a, "web", &outbound, true, &key()).unwrap();
        assert_eq!(
            cfg,
            ActionConfig::ForgeStatusReport {
                integration_id: int_id
            }
        );
    }

    #[test]
    fn build_forge_status_report_errors_on_unknown_integration() {
        let a = StateAction {
            name: "status".into(),
            action_type: "forge_status_report".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "integration": "missing" }),
        };
        let err = build_action_config(&a, "web", &HashMap::new(), true, &key()).unwrap_err();
        assert!(err.to_string().contains("missing"), "got: {err}");
    }

    #[test]
    fn build_forge_status_report_rejects_events() {
        let int_id = IntegrationId::new(Uuid::nil());
        let mut outbound = HashMap::new();
        outbound.insert("gh".to_string(), int_id);
        let a = StateAction {
            name: "status".into(),
            action_type: "forge_status_report".into(),
            active: true,
            events: vec!["build.completed".into()],
            config: serde_json::json!({ "integration": "gh" }),
        };
        let err = build_action_config(&a, "web", &outbound, true, &key()).unwrap_err();
        assert!(err.to_string().contains("events"), "got: {err}");
    }

    #[test]
    fn build_rejects_unknown_action_type() {
        let a = StateAction {
            name: "x".into(),
            action_type: "garbage".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({}),
        };
        let err = build_action_config(&a, "web", &HashMap::new(), true, &key()).unwrap_err();
        assert!(err.to_string().contains("invalid type"), "got: {err}");
    }
}
