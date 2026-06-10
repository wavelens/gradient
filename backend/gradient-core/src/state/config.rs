/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! On-disk declarative state DTOs: [`StateConfiguration`] and the per-entity
//! `State*` types it deserializes from the state JSON file. Validation lives in
//! [`super::validation`]; provisioning in [`super::provisioning`].

use gradient_types::triggers::{ConcurrencyPolicy, TriggerType};
use gradient_entity::organization_cache::CacheSubscriptionMode;
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
    /// into [`OidcGroupRoles`](super::OidcGroupRoles) and applied additively
    /// per OIDC login.
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
}
