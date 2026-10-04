/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::triggers::{ConcurrencyPolicy, TriggerType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateUser {
    pub username: String,
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub password_file: Option<String>,
    #[serde(default)]
    pub email_verified: bool,
    #[serde(default)]
    pub superuser: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProject {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub private_key_file: String,
    pub public: bool,
    #[serde(default)]
    pub hide_build_requests: bool,
    pub created_by: String,
    /// An empty list is keeping the legacy auto-add of `created_by` as Admin. A non-empty list is
    /// authoritative and revoking unmatched memberships. Members referencing unknown users are
    /// recorded as pending until registration or first OIDC login.
    #[serde(default)]
    pub members: Vec<StateProjectMemberEntry>,
    #[serde(default)]
    pub teams: Vec<StateProjectTeam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProjectMemberEntry {
    pub user: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTask {
    pub name: String,
    pub project: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub repository: String,
    #[serde(default = "default_main", alias = "evaluation_wildcard")]
    pub wildcard: String,
    #[serde(default = "default_true")]
    pub active: bool,
    pub created_by: String,
    #[serde(default = "default_keep_evaluations")]
    pub keep_evaluations: i32,
    /// `None` is leaving existing triggers untouched. `Some([])` is an error because a task needs
    /// at least one trigger.
    #[serde(default)]
    pub triggers: Option<Vec<StateTrigger>>,
    #[serde(default = "default_soft_abort")]
    pub concurrency: ConcurrencyPolicy,
    #[serde(default = "default_true")]
    pub sign_cache: bool,
    #[serde(default)]
    pub wait_for_workers: bool,
    #[serde(default)]
    pub flake_input_overrides: HashMap<String, StateFlakeInputOverride>,
    #[serde(default)]
    pub actions: Vec<StateAction>,
}

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
    #[serde(default)]
    pub integration: Option<String>,
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
    #[serde(default)]
    pub display_name: Option<String>,
    pub project: String,
    pub kind: String,
    pub git_host_type: String,
    #[serde(default)]
    pub secret_file: Option<String>,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub access_token_file: Option<String>,
    #[serde(default)]
    pub installation_id: Option<i64>,
    #[serde(default)]
    pub account_login: Option<String>,
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
    pub projects: Vec<String>,
    #[serde(default)]
    pub upstream_caches: Vec<StateUpstream>,
    pub public: bool,
    pub created_by: String,
    #[serde(default)]
    pub roles: Vec<StateCacheRoleEntry>,
    #[serde(default)]
    pub members: Vec<StateCacheMemberEntry>,
    #[serde(default)]
    pub teams: Vec<StateCacheTeam>,
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
        #[serde(default = "default_true")]
        active: bool,
    },
    External {
        display_name: String,
        url: String,
        public_key: String,
        #[serde(default = "default_true")]
        active: bool,
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
    pub permissions: Vec<String>,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateRole {
    pub name: String,
    pub project: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateWorker {
    pub worker_id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub team: Option<String>,
    pub token_file: String,
    pub display_name: String,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default = "default_true")]
    pub enable_fetch: bool,
    #[serde(default = "default_true")]
    pub enable_eval: bool,
    #[serde(default = "default_true")]
    pub enable_build: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTeam {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub members: Vec<StateTeamMember>,
    #[serde(default)]
    pub oidc_group: Option<String>,
    #[serde(default)]
    pub scim_group: Option<String>,
    #[serde(default)]
    pub new_projects: StateNewProjectGrant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTeamMember {
    pub user: String,
    pub role: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateNewProjectGrant {
    #[serde(default)]
    pub users: bool,
    #[serde(default)]
    pub workers: bool,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProjectTeam {
    pub team: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default = "default_true")]
    pub users: bool,
    #[serde(default = "default_true")]
    pub workers: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCacheTeam {
    pub team: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfiguration {
    #[serde(default)]
    pub users: HashMap<String, StateUser>,
    #[serde(default)]
    pub projects: HashMap<String, StateProject>,
    #[serde(default)]
    pub tasks: HashMap<String, StateTask>,
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
    #[serde(default)]
    pub teams: HashMap<String, StateTeam>,
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
