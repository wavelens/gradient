/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod export;
mod provisioning;

pub use export::export_state;
pub use provisioning::{
    PendingOrgMembership, PendingOrgMemberships, StateApplyResult, apply_pending_org_memberships,
};

use crate::types::{OrganizationId, RoleId};

/// Resolved at startup from [`StateRole::oidc_group`]: OIDC group name → the
/// `(organization, role)` grants a user presenting that group receives on login.
pub type OidcGroupRoles = HashMap<String, Vec<(OrganizationId, RoleId)>>;

/// Build the OIDC group → grants map from declared roles. `role_ids` maps
/// `(organization_name, role_name)` to the provisioned `(OrganizationId, RoleId)`.
pub fn resolve_oidc_group_roles(
    config: &StateConfiguration,
    role_ids: &HashMap<(String, String), (OrganizationId, RoleId)>,
) -> OidcGroupRoles {
    let mut map: OidcGroupRoles = HashMap::new();
    for role in config.roles.values() {
        if role.oidc_group.is_empty() {
            continue;
        }
        let key = (role.organization.clone(), role.name.clone());
        let Some(&grant) = role_ids.get(&key) else {
            tracing::warn!(
                organization = %role.organization,
                role = %role.name,
                "oidc_group references a role that was not provisioned; skipping",
            );
            continue;
        };
        for group in &role.oidc_group {
            map.entry(group.clone()).or_default().push(grant);
        }
    }
    map
}

use crate::ci::GITHUB_APP_INTEGRATION_NAME;
use crate::types::triggers::{ConcurrencyPolicy, TriggerType};
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateUser {
    pub username: String,
    pub name: String,
    pub email: String,
    /// Path to a credential file containing the user's plaintext password.
    /// `None` provisions an OIDC-only account (no stored password) so the
    /// OIDC login flow can claim it by email.
    #[serde(default)]
    pub password_file: Option<String>,
    #[serde(default)]
    pub email_verified: bool,
    #[serde(default)]
    pub superuser: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateOrganization {
    pub name: String,
    pub display_name: String,
    /// Explicit organization UUID. When set, a freshly created org is given
    /// this id instead of a server-generated one, so a declarative deployment
    /// can pin the value a worker references in its `peerFile`
    /// (`<org_id>:<token>`). Applied on create only; the primary key is
    /// immutable, so a value that conflicts with an existing org is rejected.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub private_key_file: String,
    pub public: bool,
    #[serde(default)]
    pub hide_build_requests: bool,
    /// GitHub App installation id to bind to this org. When `Some`, the
    /// state-driven provisioner writes it on every reconciliation (state wins
    /// over runtime updates). When `None`, the field is left untouched on
    /// update so a webhook-recorded id survives reconciliation, and is
    /// initialised to `NULL` on create.
    #[serde(default)]
    pub github_installation_id: Option<i64>,
    pub created_by: String,
    /// Declarative org membership. Empty preserves the legacy behavior of
    /// auto-adding `created_by` as Admin. Non-empty makes the list
    /// authoritative: unmatched memberships are revoked, the implicit
    /// creator-Admin assignment is skipped, and members referencing users
    /// that do not yet exist are recorded as pending and applied at
    /// registration / OIDC first-login.
    #[serde(default)]
    pub members: Vec<StateOrgMemberEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateOrgMemberEntry {
    pub user: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProject {
    pub name: String,
    pub organization: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub repository: String,
    #[serde(default = "default_main", alias = "evaluation_wildcard")]
    pub wildcard: String,
    #[serde(default = "default_true")]
    pub active: bool,
    pub created_by: String,
    /// How many evaluations to retain per project. Must be at least 1; the
    /// runtime `GRADIENT_KEEP_EVALUATIONS` cap further reduces it if exceeded.
    #[serde(default = "default_keep_evaluations")]
    pub keep_evaluations: i32,
    /// Declarative trigger list. `None` leaves existing triggers untouched
    /// (back-compat). `Some([])` is an error - a project must have at least one.
    #[serde(default)]
    pub triggers: Option<Vec<StateTrigger>>,
    /// Concurrency policy for this project. Defaults to `soft_abort` when omitted.
    #[serde(default = "default_soft_abort")]
    pub concurrency: ConcurrencyPolicy,
    /// When `false`, build outputs from this project are pushed to the cache
    /// but their narinfo signatures are left empty, so external Nix clients
    /// won't trust them. Defaults to `true`.
    #[serde(default = "default_true")]
    pub sign_cache: bool,
    /// Declarative flake input overrides. An absent or empty map deletes all
    /// existing override rows for this project.
    #[serde(default)]
    pub flake_input_overrides: HashMap<String, StateFlakeInputOverride>,
    /// Declarative action list. Re-applying state with fewer actions removes
    /// the missing ones (matched by `name` within the project).
    #[serde(default)]
    pub actions: Vec<StateAction>,
}

/// Declarative project action. `config` is type-specific and validated
/// against `action_type` at apply time:
///   - `send_mail`           `{ recipients: [..], subject_template?: str }`
///   - `send_web_request`    `{ url: str, token_file?: str }`
///   - `forge_status_report` `{ integration: <outbound integration name> }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StateAction {
    pub name: String,
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub events: Vec<String>,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFlakeInputOverride {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub keep_url: bool,
}

fn default_soft_abort() -> ConcurrencyPolicy {
    ConcurrencyPolicy::SoftAbort
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateTrigger {
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    /// Name of an inbound integration in the same org. Required for
    /// `reporter_push` and `reporter_pull_request` triggers.
    #[serde(default)]
    pub integration: Option<String>,
    /// Type-specific config shape:
    /// - polling: `{ interval_secs }`
    /// - reporter_push: `{ branches, tags, releases_only }`
    /// - reporter_pull_request: `{ branches, actions }`
    /// - time: `{ cron }`
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default = "default_active")]
    pub active: bool,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateIntegration {
    pub name: String,
    /// Defaults to `name` when unset.
    #[serde(default)]
    pub display_name: Option<String>,
    pub organization: String,
    /// `"inbound"` or `"outbound"`.
    pub kind: String,
    /// `"gitea"`, `"forgejo"`, `"gitlab"`, or `"github"`.
    pub forge_type: String,
    #[serde(default)]
    pub secret_file: Option<String>,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub access_token_file: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCache {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub local_priority: Option<i32>,
    #[serde(default)]
    pub max_storage_gb: i32,
    pub signing_key_file: String,
    #[serde(default)]
    pub organizations: Vec<String>,
    #[serde(default)]
    pub upstreams: Vec<StateUpstream>,
    pub public: bool,
    pub created_by: String,
    #[serde(default)]
    pub roles: Vec<StateCacheRoleEntry>,
    #[serde(default)]
    pub members: Vec<StateCacheMemberEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCacheRoleEntry {
    pub name: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCacheMemberEntry {
    pub user: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StateUpstream {
    Internal {
        cache_name: String,
        display_name: Option<String>,
        #[serde(default = "default_upstream_mode")]
        mode: CacheSubscriptionMode,
    },
    External {
        display_name: String,
        url: String,
        public_key: String,
    },
}

fn default_upstream_mode() -> CacheSubscriptionMode {
    CacheSubscriptionMode::ReadWrite
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateApiKey {
    pub name: String,
    pub key_file: String,
    pub owned_by: String,
    /// Capability identifiers (matching `Permission::as_wire_name`) the key
    /// should grant. Required - there is no safe default.
    pub permissions: Vec<String>,
    /// Optional organization name to pin the key to. `None` = unscoped.
    #[serde(default)]
    pub organization: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateRole {
    pub name: String,
    /// Organization the role belongs to. State-managed roles are always
    /// org-scoped - there is no way to define a global state-managed role.
    pub organization: String,
    /// Capability identifiers (matching `Permission::as_wire_name`) the role
    /// grants. Required - there is no safe default.
    pub permissions: Vec<String>,
    /// OIDC group claims that grant this role on login. Resolved at startup
    /// into [`OidcGroupRoles`] and applied additively per OIDC login.
    #[serde(default)]
    pub oidc_group: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateWorker {
    pub worker_id: String,
    #[serde(default)]
    pub url: Option<String>,
    /// Organizations the worker is registered under. One
    /// `worker_registration` row is provisioned per (worker_id, org)
    /// pair so the same physical worker can serve builds for multiple
    /// orgs without duplicating the declarative entry.
    pub organizations: Vec<String>,
    pub token_file: String,
    /// Human-readable display name shown in the workers list.
    pub display_name: String,
    pub created_by: String,
    /// Per-registration server-side gate for `fetch`. Defaults to true.
    #[serde(default = "default_true")]
    pub enable_fetch: bool,
    /// Per-registration server-side gate for `eval`. Defaults to true.
    #[serde(default = "default_true")]
    pub enable_eval: bool,
    /// Per-registration server-side gate for `build`. Defaults to true.
    #[serde(default = "default_true")]
    pub enable_build: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfiguration {
    #[serde(default)]
    pub users: HashMap<String, StateUser>,
    #[serde(default)]
    pub organizations: HashMap<String, StateOrganization>,
    #[serde(default)]
    pub projects: HashMap<String, StateProject>,
    #[serde(default)]
    pub caches: HashMap<String, StateCache>,
    #[serde(default)]
    pub roles: HashMap<String, StateRole>,
    #[serde(default)]
    pub api_keys: HashMap<String, StateApiKey>,
    #[serde(default)]
    pub workers: HashMap<String, StateWorker>,
    #[serde(default)]
    pub integrations: HashMap<String, StateIntegration>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("Validation error in field '{field}': {message}")]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
    pub is_valid: bool,
}

fn default_true() -> bool {
    true
}

fn default_main() -> String {
    "main".to_string()
}

fn default_priority() -> i32 {
    10
}

fn default_keep_evaluations() -> i32 {
    30
}

impl StateConfiguration {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let config: StateConfiguration = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn validate(&self) -> ValidationResult {
        let mut errors = Vec::new();

        for user in self.users.values() {
            if !user.email.contains('@') {
                errors.push(ValidationError {
                    field: format!("users.{}.email", user.username),
                    message: "Invalid email format".to_string(),
                });
            }
        }

        let mut org_ids_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for org in self.organizations.values() {
            if !self.users.contains_key(&org.created_by) {
                errors.push(ValidationError {
                    field: format!("organizations.{}.created_by", org.name),
                    message: format!("User '{}' does not exist", org.created_by),
                });
            }

            if let Some(id) = &org.id {
                match id.trim().parse::<uuid::Uuid>() {
                    Ok(parsed) => {
                        if !org_ids_seen.insert(parsed.to_string()) {
                            errors.push(ValidationError {
                                field: format!("organizations.{}.id", org.name),
                                message: format!("Duplicate organization id '{}'", id),
                            });
                        }
                    }
                    Err(_) => errors.push(ValidationError {
                        field: format!("organizations.{}.id", org.name),
                        message: format!("Invalid UUID '{}'", id),
                    }),
                }
            }

            let declared_org_role_names: std::collections::HashSet<&str> = self
                .roles
                .values()
                .filter(|r| r.organization == org.name)
                .map(|r| r.name.as_str())
                .collect();
            let mut member_users_seen: std::collections::HashSet<&str> =
                std::collections::HashSet::new();
            for member in &org.members {
                let builtin = matches!(member.role.as_str(), "Admin" | "Write" | "View");
                if !builtin && !declared_org_role_names.contains(member.role.as_str()) {
                    errors.push(ValidationError {
                        field: format!("organizations.{}.members.{}.role", org.name, member.user),
                        message: format!(
                            "Role '{}' not found for organization '{}' (must be Admin/Write/View or a state-managed org role)",
                            member.role, org.name
                        ),
                    });
                }
                if !member_users_seen.insert(member.user.as_str()) {
                    errors.push(ValidationError {
                        field: format!("organizations.{}.members.{}.user", org.name, member.user),
                        message: format!(
                            "Duplicate member entry for user '{}' in organization '{}'",
                            member.user, org.name
                        ),
                    });
                }
                // Note: missing user is intentionally not an error (issue #94).
            }
        }

        for project in self.projects.values() {
            if !self.organizations.contains_key(&project.organization) {
                errors.push(ValidationError {
                    field: format!("projects.{}.organization", project.name),
                    message: format!("Organization '{}' does not exist", project.organization),
                });
            }

            if !self.users.contains_key(&project.created_by) {
                errors.push(ValidationError {
                    field: format!("projects.{}.created_by", project.name),
                    message: format!("User '{}' does not exist", project.created_by),
                });
            }

            if !project.repository.starts_with("http") && !project.repository.starts_with("git") {
                errors.push(ValidationError {
                    field: format!("projects.{}.repository", project.name),
                    message: "Repository URL must start with http or git".to_string(),
                });
            }

            if project.keep_evaluations < 1 {
                errors.push(ValidationError {
                    field: format!("projects.{}.keep_evaluations", project.name),
                    message: "keep_evaluations must be at least 1".to_string(),
                });
            }

            let mut action_names: std::collections::HashSet<&str> =
                std::collections::HashSet::new();
            for action in &project.actions {
                if !matches!(
                    action.action_type.as_str(),
                    "send_mail" | "send_web_request" | "forge_status_report"
                ) {
                    errors.push(ValidationError {
                        field: format!("projects.{}.actions.{}.type", project.name, action.name),
                        message: format!(
                            "Invalid action type '{}': expected send_mail/send_web_request/forge_status_report",
                            action.action_type
                        ),
                    });
                }
                if action.action_type == "forge_status_report" && !action.events.is_empty() {
                    errors.push(ValidationError {
                        field: format!("projects.{}.actions.{}.events", project.name, action.name),
                        message: "forge_status_report actions cannot carry custom events".into(),
                    });
                }
                if !action_names.insert(action.name.as_str()) {
                    errors.push(ValidationError {
                        field: format!("projects.{}.actions.{}.name", project.name, action.name),
                        message: format!(
                            "Duplicate action name '{}' in project '{}'",
                            action.name, project.name
                        ),
                    });
                }
            }

            // Reporter triggers resolve their `integration` against the org's
            // inbound integrations at apply time; catch a missing/outbound/typo
            // reference here so it fails validation instead of mid-apply (#332).
            for trigger in project.triggers.iter().flatten() {
                if !matches!(
                    trigger.trigger_type,
                    TriggerType::ReporterPush | TriggerType::ReporterPullRequest
                ) {
                    continue;
                }
                let Some(name) = &trigger.integration else {
                    errors.push(ValidationError {
                        field: format!("projects.{}.triggers", project.name),
                        message: "reporter_push/reporter_pull_request triggers require an `integration`".into(),
                    });
                    continue;
                };
                if name == GITHUB_APP_INTEGRATION_NAME {
                    continue;
                }
                let declared_inbound = self.integrations.values().any(|i| {
                    i.name == *name
                        && i.organization == project.organization
                        && i.kind == "inbound"
                });
                if !declared_inbound {
                    errors.push(ValidationError {
                        field: format!("projects.{}.triggers", project.name),
                        message: format!(
                            "Reporter trigger references integration '{}' which is not a declared inbound integration in organization '{}'",
                            name, project.organization
                        ),
                    });
                }
            }
        }

        for integration in self.integrations.values() {
            if !self.organizations.contains_key(&integration.organization) {
                errors.push(ValidationError {
                    field: format!("integrations.{}.organization", integration.name),
                    message: format!("Organization '{}' does not exist", integration.organization),
                });
            }
            if !self.users.contains_key(&integration.created_by) {
                errors.push(ValidationError {
                    field: format!("integrations.{}.created_by", integration.name),
                    message: format!("User '{}' does not exist", integration.created_by),
                });
            }
            if !matches!(integration.kind.as_str(), "inbound" | "outbound") {
                errors.push(ValidationError {
                    field: format!("integrations.{}.kind", integration.name),
                    message: format!(
                        "Invalid kind '{}': expected 'inbound' or 'outbound'",
                        integration.kind
                    ),
                });
            }
            if !matches!(
                integration.forge_type.as_str(),
                "gitea" | "forgejo" | "gitlab" | "github"
            ) {
                errors.push(ValidationError {
                    field: format!("integrations.{}.forge_type", integration.name),
                    message: format!(
                        "Invalid forge_type '{}': expected gitea/forgejo/gitlab/github",
                        integration.forge_type
                    ),
                });
            }
        }

        for cache in self.caches.values() {
            if !self.users.contains_key(&cache.created_by) {
                errors.push(ValidationError {
                    field: format!("caches.{}.created_by", cache.name),
                    message: format!("User '{}' does not exist", cache.created_by),
                });
            }

            for org_name in &cache.organizations {
                if !self.organizations.contains_key(org_name) {
                    errors.push(ValidationError {
                        field: format!("caches.{}.organizations", cache.name),
                        message: format!("Organization '{}' does not exist", org_name),
                    });
                }
            }

            let mut role_names_seen: std::collections::HashSet<&str> =
                std::collections::HashSet::new();
            for role in &cache.roles {
                if matches!(role.name.as_str(), "Admin" | "Write" | "View") {
                    errors.push(ValidationError {
                        field: format!("caches.{}.roles.{}.name", cache.name, role.name),
                        message: format!(
                            "Role name '{}' collides with a built-in cache role; pick a different name.",
                            role.name
                        ),
                    });
                }
                if role.permissions.is_empty() {
                    errors.push(ValidationError {
                        field: format!("caches.{}.roles.{}.permissions", cache.name, role.name),
                        message: "At least one permission must be declared.".into(),
                    });
                }
                for wire in &role.permissions {
                    if crate::permissions::CachePermission::from_wire_name(wire).is_none() {
                        errors.push(ValidationError {
                            field: format!("caches.{}.roles.{}.permissions", cache.name, role.name),
                            message: format!("Unknown cache permission '{}'", wire),
                        });
                    }
                }
                if !role_names_seen.insert(role.name.as_str()) {
                    errors.push(ValidationError {
                        field: format!("caches.{}.roles.{}.name", cache.name, role.name),
                        message: format!(
                            "Duplicate role name '{}' in cache '{}'",
                            role.name, cache.name
                        ),
                    });
                }
            }

            let declared_role_names: std::collections::HashSet<&str> =
                cache.roles.iter().map(|r| r.name.as_str()).collect();
            for member in &cache.members {
                if !self.users.contains_key(&member.user) {
                    errors.push(ValidationError {
                        field: format!("caches.{}.members.{}.user", cache.name, member.user),
                        message: format!("User '{}' does not exist", member.user),
                    });
                }
                let builtin = matches!(member.role.as_str(), "Admin" | "Write" | "View");
                if !builtin && !declared_role_names.contains(member.role.as_str()) {
                    errors.push(ValidationError {
                        field: format!("caches.{}.members.{}.role", cache.name, member.user),
                        message: format!(
                            "Role '{}' not found in cache '{}'",
                            member.role, cache.name
                        ),
                    });
                }
            }
        }

        let mut role_keys_seen_per_org: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for role in self.roles.values() {
            if !self.organizations.contains_key(&role.organization) {
                errors.push(ValidationError {
                    field: format!("roles.{}.organization", role.name),
                    message: format!("Organization '{}' does not exist", role.organization),
                });
            }
            if role.permissions.is_empty() {
                errors.push(ValidationError {
                    field: format!("roles.{}.permissions", role.name),
                    message: "At least one permission must be declared.".into(),
                });
            }
            for wire in &role.permissions {
                if crate::permissions::Permission::from_wire_name(wire).is_none() {
                    errors.push(ValidationError {
                        field: format!("roles.{}.permissions", role.name),
                        message: format!("Unknown permission '{}'", wire),
                    });
                }
            }
            if matches!(role.name.as_str(), "Admin" | "Write" | "View") {
                errors.push(ValidationError {
                    field: format!("roles.{}.name", role.name),
                    message: format!(
                        "Role name '{}' collides with a built-in role; pick a different name.",
                        role.name
                    ),
                });
            }
            let key = (role.organization.clone(), role.name.clone());
            if !role_keys_seen_per_org.insert(key) {
                errors.push(ValidationError {
                    field: format!("roles.{}.name", role.name),
                    message: format!(
                        "Duplicate role '{}' in organization '{}'",
                        role.name, role.organization
                    ),
                });
            }
        }

        for api_key in self.api_keys.values() {
            if !self.users.contains_key(&api_key.owned_by) {
                errors.push(ValidationError {
                    field: format!("api_keys.{}.owned_by", api_key.name),
                    message: format!("User '{}' does not exist", api_key.owned_by),
                });
            }
            if api_key.permissions.is_empty() {
                errors.push(ValidationError {
                    field: format!("api_keys.{}.permissions", api_key.name),
                    message: "At least one permission must be declared.".into(),
                });
            }
            for wire in &api_key.permissions {
                if crate::permissions::Permission::from_wire_name(wire).is_none() {
                    errors.push(ValidationError {
                        field: format!("api_keys.{}.permissions", api_key.name),
                        message: format!("Unknown permission '{}'", wire),
                    });
                }
            }
            if let Some(org) = &api_key.organization
                && !self.organizations.contains_key(org)
            {
                errors.push(ValidationError {
                    field: format!("api_keys.{}.organization", api_key.name),
                    message: format!("Organization '{}' does not exist", org),
                });
            }
        }

        for worker in self.workers.values() {
            if worker.organizations.is_empty() {
                errors.push(ValidationError {
                    field: format!("workers.{}.organizations", worker.worker_id),
                    message: "Worker must be registered under at least one organization".into(),
                });
            }
            for org in &worker.organizations {
                if !self.organizations.contains_key(org) {
                    errors.push(ValidationError {
                        field: format!("workers.{}.organizations", worker.worker_id),
                        message: format!("Organization '{}' does not exist", org),
                    });
                }
            }
            if !self.users.contains_key(&worker.created_by) {
                errors.push(ValidationError {
                    field: format!("workers.{}.created_by", worker.worker_id),
                    message: format!("User '{}' does not exist", worker.created_by),
                });
            }
        }

        ValidationResult {
            is_valid: errors.is_empty(),
            errors,
        }
    }
}

/// Load and validate a state file without touching the database. Returns the
/// human-readable validation errors (empty `Vec` = valid). Backs the
/// `--validate-state` CLI flag so config mistakes surface at build/CI time
/// instead of on server start.
pub fn validate_state_file(path: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let config = StateConfiguration::from_file(path)?;
    Ok(config
        .validate()
        .errors
        .into_iter()
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect())
}

pub async fn load_and_apply_state(
    db: &DatabaseConnection,
    state_file_path: Option<&str>,
    crypt_secret_file: &str,
    delete_state: bool,
    email_enabled: bool,
) -> Result<StateApplyResult, Box<dyn std::error::Error>> {
    let Some(path) = state_file_path else {
        tracing::info!("No state file configured, skipping state management");
        return Ok(StateApplyResult {
            pending: PendingOrgMemberships::new(),
            oidc_group_roles: OidcGroupRoles::new(),
        });
    };

    tracing::info!(path, "Loading state configuration");

    let config = StateConfiguration::from_file(path)?;

    let validation = config.validate();
    if !validation.is_valid {
        let error_messages: Vec<String> = validation
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect();

        return Err(format!(
            "State configuration validation failed:\n{}",
            error_messages.join("\n")
        )
        .into());
    }

    tracing::info!("State configuration validated successfully");

    let result = provisioning::apply_state_to_database(
        db,
        &config,
        crypt_secret_file,
        delete_state,
        email_enabled,
    )
    .await?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_accepts_missing_password_file() {
        // OIDC-only users have `password_file = null`; serde must default to
        // None instead of failing. This is the on-disk contract that lets
        // gradient-state.nix emit `password_file = null` entries.
        let json = r#"{
            "users": {
                "alice": {
                    "username": "alice",
                    "name": "Alice",
                    "email": "alice@example.com",
                    "password_file": null,
                    "email_verified": true,
                    "superuser": false
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert!(cfg.users["alice"].password_file.is_none());
    }

    #[test]
    fn org_project_cache_descriptions_optional() {
        let json = r#"{
            "users": {
                "alice": {
                    "username": "alice",
                    "name": "Alice",
                    "email": "alice@example.com",
                    "password_file": "/dev/null"
                }
            },
            "organizations": {
                "acme": {
                    "name": "acme",
                    "display_name": "ACME",
                    "description": null,
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            },
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice"
                }
            },
            "caches": {
                "main": {
                    "name": "main",
                    "display_name": "Main",
                    "signing_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert!(cfg.organizations["acme"].description.is_none());
        assert!(cfg.projects["web"].description.is_none());
        assert!(cfg.caches["main"].description.is_none());
        assert!(cfg.validate().is_valid);
    }

    #[test]
    fn state_project_concurrency_defaults_to_soft_abort() {
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(
            cfg.projects["web"].concurrency,
            ConcurrencyPolicy::SoftAbort
        );
    }

    #[test]
    fn state_project_accepts_wildcard_field() {
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "wildcard": "packages.x86_64-linux.*",
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.projects["web"].wildcard, "packages.x86_64-linux.*");
    }

    #[test]
    fn state_project_accepts_legacy_evaluation_wildcard_alias() {
        // Existing nix configurations using `evaluation_wildcard` must keep
        // working after the rename to `wildcard`.
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "evaluation_wildcard": "checks.*",
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.projects["web"].wildcard, "checks.*");
    }

    #[test]
    fn state_project_keep_evaluations_defaults_to_thirty() {
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.projects["web"].keep_evaluations, 30);
    }

    #[test]
    fn state_project_keep_evaluations_zero_rejected_by_validator() {
        let json = r#"{
            "users": {
                "alice": {
                    "username": "alice",
                    "name": "Alice",
                    "email": "alice@example.com",
                    "password_file": "/dev/null"
                }
            },
            "organizations": {
                "acme": {
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            },
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice",
                    "keep_evaluations": 0
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors
                .iter()
                .any(|e| e.field == "projects.web.keep_evaluations"
                    && e.message.contains("at least 1")),
            "expected keep_evaluations >= 1 validation error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_project_actions_round_trip_all_types() {
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice",
                    "actions": [
                        {
                            "name": "notify-ops",
                            "type": "send_mail",
                            "events": ["build.failed"],
                            "config": { "recipients": ["ops@example.com"] }
                        },
                        {
                            "name": "webhook",
                            "type": "send_web_request",
                            "events": ["build.completed"],
                            "config": { "url": "https://hooks.example.com/gradient", "token_file": "/etc/gradient/secrets/hook-token" }
                        },
                        {
                            "name": "status",
                            "type": "forge_status_report",
                            "config": { "integration": "gitea-prod" }
                        }
                    ]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.projects["web"].actions.len(), 3);
        assert_eq!(cfg.projects["web"].actions[0].action_type, "send_mail");
        assert!(cfg.projects["web"].actions[2].events.is_empty());
    }

    fn reporter_cfg(integration_name: &str, integrations_json: &str) -> StateConfiguration {
        let json = format!(
            r#"{{
                "users": {{
                    "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
                }},
                "organizations": {{
                    "acme": {{ "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice" }}
                }},
                "integrations": {integrations_json},
                "projects": {{
                    "web": {{
                        "name": "web", "organization": "acme", "display_name": "Web",
                        "repository": "https://example.com/acme/web.git", "created_by": "alice",
                        "triggers": [
                            {{ "type": "reporter_push", "integration": "{integration_name}", "config": {{ "branches": ["main"] }} }}
                        ]
                    }}
                }}
            }}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn state_reporter_trigger_accepts_declared_inbound_integration() {
        let integrations = r#"{
            "forge": { "name": "forge", "organization": "acme", "kind": "inbound", "forge_type": "forgejo", "created_by": "alice" }
        }"#;
        let cfg = reporter_cfg("forge", integrations);
        let v = cfg.validate();
        assert!(v.is_valid, "errors: {:?}", v.errors);
    }

    #[test]
    fn state_reporter_trigger_rejects_unknown_integration() {
        let cfg = reporter_cfg("ghost", "{}");
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors.iter().any(|e| e.field == "projects.web.triggers"
                && e.message.contains("ghost")),
            "expected unknown-integration trigger error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_reporter_trigger_rejects_outbound_integration() {
        let integrations = r#"{
            "forge": { "name": "forge", "organization": "acme", "kind": "outbound", "forge_type": "forgejo", "created_by": "alice" }
        }"#;
        let cfg = reporter_cfg("forge", integrations);
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors.iter().any(|e| e.field == "projects.web.triggers"),
            "expected outbound-integration trigger error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_reporter_trigger_accepts_github_app_name() {
        let cfg = reporter_cfg("github", "{}");
        let v = cfg.validate();
        assert!(v.is_valid, "errors: {:?}", v.errors);
    }

    #[test]
    fn state_action_rejects_unknown_field() {
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice",
                    "actions": [
                        {
                            "name": "x",
                            "type": "send_mail",
                            "events": [],
                            "config": {},
                            "bogus": true
                        }
                    ]
                }
            }
        }"#;
        let err = serde_json::from_str::<StateConfiguration>(json).unwrap_err();
        assert!(err.to_string().contains("bogus"), "got: {err}");
    }

    #[test]
    fn state_action_validate_rejects_unknown_type() {
        let json = r#"{
            "users": {
                "alice": {
                    "username": "alice", "name": "Alice", "email": "a@x.io",
                    "password_file": "/dev/null"
                }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice"
                }
            },
            "projects": {
                "web": {
                    "name": "web", "organization": "acme", "display_name": "Web",
                    "repository": "https://example.com/acme/web.git", "created_by": "alice",
                    "actions": [
                        { "name": "a", "type": "garbage", "config": {} }
                    ]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors
                .iter()
                .any(|e| e.field == "projects.web.actions.a.type"),
            "expected unknown-type error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_action_validate_rejects_duplicate_names() {
        let json = r#"{
            "users": {
                "alice": {
                    "username": "alice", "name": "Alice", "email": "a@x.io",
                    "password_file": "/dev/null"
                }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice"
                }
            },
            "projects": {
                "web": {
                    "name": "web", "organization": "acme", "display_name": "Web",
                    "repository": "https://example.com/acme/web.git", "created_by": "alice",
                    "actions": [
                        { "name": "dup", "type": "send_mail", "config": { "recipients": ["a@x.io"] } },
                        { "name": "dup", "type": "send_mail", "config": { "recipients": ["b@x.io"] } }
                    ]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors
                .iter()
                .any(|e| e.message.contains("Duplicate action name")),
            "expected duplicate-name error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_action_validate_rejects_events_on_forge_status_report() {
        let json = r#"{
            "users": {
                "alice": {
                    "username": "alice", "name": "Alice", "email": "a@x.io",
                    "password_file": "/dev/null"
                }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice"
                }
            },
            "projects": {
                "web": {
                    "name": "web", "organization": "acme", "display_name": "Web",
                    "repository": "https://example.com/acme/web.git", "created_by": "alice",
                    "actions": [
                        { "name": "x", "type": "forge_status_report", "events": ["build.completed"], "config": { "integration": "gh" } }
                    ]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors
                .iter()
                .any(|e| e.field == "projects.web.actions.x.events"),
            "expected forge_status_report-events error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_project_silently_ignores_legacy_force_evaluation_field() {
        // Old state files may still set `force_evaluation` - serde drops
        // unknown fields by default, so parsing must keep working.
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice",
                    "force_evaluation": true
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.projects["web"].name, "web");
    }

    #[test]
    fn state_project_concurrency_hard_abort_round_trip() {
        let json = r#"{
            "projects": {
                "web": {
                    "name": "web",
                    "organization": "acme",
                    "display_name": "Web",
                    "repository": "https://example.com/acme/web.git",
                    "created_by": "alice",
                    "concurrency": "hard_abort"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(
            cfg.projects["web"].concurrency,
            ConcurrencyPolicy::HardAbort
        );
        assert_eq!(i16::from(cfg.projects["web"].concurrency), 0);
    }

    fn worker_cfg(orgs_json: &str) -> StateConfiguration {
        let json = format!(
            r#"{{
                "users": {{
                    "alice": {{
                        "username": "alice",
                        "name": "Alice",
                        "email": "alice@example.com",
                        "password_file": "/dev/null"
                    }}
                }},
                "organizations": {{
                    "acme": {{
                        "name": "acme",
                        "display_name": "ACME",
                        "private_key_file": "/dev/null",
                        "public": false,
                        "created_by": "alice"
                    }},
                    "globex": {{
                        "name": "globex",
                        "display_name": "Globex",
                        "private_key_file": "/dev/null",
                        "public": false,
                        "created_by": "alice"
                    }}
                }},
                "workers": {{
                    "builder-1": {{
                        "worker_id": "550e8400-e29b-41d4-a716-446655440001",
                        "organizations": {orgs_json},
                        "token_file": "/dev/null",
                        "display_name": "Primary Build Server",
                        "created_by": "alice"
                    }}
                }}
            }}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn state_worker_accepts_multiple_organizations() {
        let cfg = worker_cfg(r#"["acme", "globex"]"#);
        assert_eq!(
            cfg.workers["builder-1"].organizations,
            vec!["acme".to_owned(), "globex".to_owned()]
        );
        assert!(cfg.validate().is_valid);
    }

    #[test]
    fn state_worker_rejects_empty_organizations() {
        let cfg = worker_cfg("[]");
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors.iter().any(|e| e.field
                == "workers.550e8400-e29b-41d4-a716-446655440001.organizations"
                && e.message.contains("at least one")),
            "expected at-least-one-org error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_org_accepts_explicit_id() {
        let json = r#"{
            "organizations": {
                "acme": {
                    "name": "acme",
                    "display_name": "ACME",
                    "id": "018f6f3a-0000-7000-8000-000000000001",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(
            cfg.organizations["acme"].id.as_deref(),
            Some("018f6f3a-0000-7000-8000-000000000001")
        );
    }

    #[test]
    fn state_org_id_defaults_none() {
        let json = r#"{
            "organizations": {
                "acme": {
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert!(cfg.organizations["acme"].id.is_none());
    }

    #[test]
    fn state_org_validator_rejects_malformed_id() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME", "id": "not-a-uuid",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors.iter().any(|e| e.field == "organizations.acme.id"),
            "expected invalid-id error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_org_validator_rejects_duplicate_ids() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME", "id": "018f6f3a-0000-7000-8000-000000000001",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice"
                },
                "globex": {
                    "name": "globex", "display_name": "Globex", "id": "018f6f3a-0000-7000-8000-000000000001",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors.iter().any(|e| e.message.contains("Duplicate organization id")),
            "expected duplicate-id error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_org_members_serde_round_trip() {
        let json = r#"{
            "organizations": {
                "acme": {
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice",
                    "members": [
                        { "user": "bob", "role": "Write" },
                        { "user": "carol", "role": "releaser" }
                    ]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let members = &cfg.organizations["acme"].members;
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].user, "bob");
        assert_eq!(members[0].role, "Write");
        assert_eq!(members[1].user, "carol");
        assert_eq!(members[1].role, "releaser");
    }

    #[test]
    fn state_org_members_default_empty() {
        let json = r#"{
            "organizations": {
                "acme": {
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        assert!(cfg.organizations["acme"].members.is_empty());
    }

    #[test]
    fn state_org_members_validator_accepts_builtin_role() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" },
                "bob":   { "username": "bob",   "name": "Bob",   "email": "b@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                    "members": [{ "user": "bob", "role": "Write" }]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(v.is_valid, "errors: {:?}", v.errors);
    }

    #[test]
    fn state_org_members_validator_accepts_custom_org_role() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                    "members": [{ "user": "alice", "role": "releaser" }]
                }
            },
            "roles": {
                "releaser": { "name": "releaser", "organization": "acme", "permissions": ["viewOrg"] }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(v.is_valid, "errors: {:?}", v.errors);
    }

    #[test]
    fn state_org_members_validator_rejects_unknown_role() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                    "members": [{ "user": "alice", "role": "Ghost" }]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors
                .iter()
                .any(|e| e.field == "organizations.acme.members.alice.role"),
            "expected unknown-role error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_org_members_validator_ignores_unknown_user() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                    "members": [{ "user": "ghost", "role": "Write" }]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(
            v.is_valid,
            "missing user must not fail validation (issue #94): {:?}",
            v.errors
        );
    }

    #[test]
    fn state_org_members_validator_rejects_duplicate_user() {
        let json = r#"{
            "users": {
                "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
            },
            "organizations": {
                "acme": {
                    "name": "acme", "display_name": "ACME",
                    "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                    "members": [
                        { "user": "alice", "role": "Write" },
                        { "user": "alice", "role": "View" }
                    ]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors
                .iter()
                .any(|e| e.message.contains("Duplicate member")),
            "expected duplicate-member error, got: {:?}",
            v.errors
        );
    }

    #[test]
    fn state_worker_rejects_unknown_organization_in_list() {
        let cfg = worker_cfg(r#"["acme", "ghost"]"#);
        let v = cfg.validate();
        assert!(!v.is_valid);
        assert!(
            v.errors.iter().any(|e| e.field
                == "workers.550e8400-e29b-41d4-a716-446655440001.organizations"
                && e.message.contains("'ghost'")),
            "expected unknown-org error mentioning 'ghost', got: {:?}",
            v.errors
        );
    }

    #[test]
    fn resolves_group_to_org_role_grants() {
        let json = r#"{
            "roles": {
                "platform": {
                    "name": "platform-admin",
                    "organization": "acme",
                    "permissions": ["create_project"],
                    "oidc_group": ["platform-team", "ops"]
                },
                "unmapped": {
                    "name": "viewer",
                    "organization": "acme",
                    "permissions": ["view_project"]
                }
            }
        }"#;
        let cfg: StateConfiguration = serde_json::from_str(json).unwrap();

        let org = OrganizationId::now_v7();
        let role = RoleId::now_v7();
        let mut role_ids = HashMap::new();
        role_ids.insert(("acme".to_string(), "platform-admin".to_string()), (org, role));
        role_ids.insert(
            ("acme".to_string(), "viewer".to_string()),
            (org, RoleId::now_v7()),
        );

        let resolved = resolve_oidc_group_roles(&cfg, &role_ids);
        assert_eq!(resolved.get("platform-team"), Some(&vec![(org, role)]));
        assert_eq!(resolved.get("ops"), Some(&vec![(org, role)]));
        assert!(!resolved.contains_key("unmapped"));
    }
}
