/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Reconstruct a [`StateConfiguration`] from the live database - the inverse of
//! the provisioner in [`super::provisioning`]. Powers `GET /admin/state`, which
//! lets operators read the current users / orgs / projects / caches / etc. back
//! into a declarative `services.gradient.state` block.
//!
//! Secrets are never recoverable from the DB (passwords and worker tokens are
//! hashed, signing keys and integration secrets are encrypted). Every `*_file`
//! field is therefore redacted to `null` by [`redact`] before serialization, so
//! the operator fills in the credential paths.

use super::{
    StateApiKey, StateCache, StateCacheMemberEntry, StateCacheRoleEntry, StateConfiguration,
    StateFlakeInputOverride, StateIntegration, StateOrgMemberEntry, StateOrganization,
    StateProject, StateRole, StateTrigger, StateUpstream, StateUser, StateWorker,
};
use crate::ci::{ForgeType, IntegrationKind};
use crate::permissions::{cache_mask_to_vec, is_builtin_cache_role, is_builtin_role, mask_to_vec};
use crate::types::actions::{ActionConfig, ActionType};
use crate::types::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerType};
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::ids::*;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};
use std::collections::HashMap;

/// JSON keys that name a credential file. Redacted to `null` everywhere they
/// appear in the serialized state, regardless of nesting depth.
const SECRET_KEYS: &[&str] = &[
    "password_file",
    "private_key_file",
    "signing_key_file",
    "token_file",
    "key_file",
    "secret_file",
    "access_token_file",
];

/// Build the full declarative state from every relevant table.
///
/// This is a snapshot of the live system, not just state-managed rows: every
/// user, org, project, cache, custom role, api key, worker and integration is
/// included so the operator can codify the current system into nix. Rows the
/// operator cannot hand-author are excluded - the auto-managed `build-request`
/// project, the server-managed GitHub integration rows, and the built-in
/// `Admin`/`Write`/`View` roles.
pub async fn export_state<C: ConnectionTrait>(db: &C) -> Result<StateConfiguration, DbErr> {
    let users = gradient_entity::user::Entity::find().all(db).await?;
    let orgs = gradient_entity::organization::Entity::find().all(db).await?;
    let projects = gradient_entity::project::Entity::find().all(db).await?;
    let caches = gradient_entity::cache::Entity::find().all(db).await?;
    let roles = gradient_entity::role::Entity::find().all(db).await?;
    let cache_roles = gradient_entity::cache_role::Entity::find().all(db).await?;
    let api_keys = gradient_entity::api::Entity::find().all(db).await?;
    let registrations = gradient_entity::worker_registration::Entity::find().all(db).await?;
    let integrations = gradient_entity::integration::Entity::find().all(db).await?;
    let org_users = gradient_entity::organization_user::Entity::find().all(db).await?;
    let cache_users = gradient_entity::cache_user::Entity::find().all(db).await?;
    let org_caches = gradient_entity::organization_cache::Entity::find().all(db).await?;
    let upstreams = gradient_entity::cache_upstream::Entity::find().all(db).await?;
    let triggers = gradient_entity::project_trigger::Entity::find().all(db).await?;
    let actions = gradient_entity::project_action::Entity::find().all(db).await?;
    let overrides = gradient_entity::project_flake_input_override::Entity::find().all(db).await?;

    let username = id_name_map(users.iter().map(|u| (u.id, u.username.clone())));
    let org_name = id_name_map(orgs.iter().map(|o| (o.id, o.name.clone())));
    let cache_name = id_name_map(caches.iter().map(|c| (c.id, c.name.clone())));
    let role_name = id_name_map(roles.iter().map(|r| (r.id, r.name.clone())));
    let cache_role_name = id_name_map(cache_roles.iter().map(|r| (r.id, r.name.clone())));
    let integration_name = id_name_map(integrations.iter().map(|i| (i.id, i.name.clone())));

    let mut config = StateConfiguration {
        users: HashMap::new(),
        organizations: HashMap::new(),
        projects: HashMap::new(),
        caches: HashMap::new(),
        roles: HashMap::new(),
        api_keys: HashMap::new(),
        workers: HashMap::new(),
        integrations: HashMap::new(),
    };

    for u in &users {
        config.users.insert(
            u.username.clone(),
            StateUser {
                username: u.username.clone(),
                name: u.name.clone(),
                email: u.email.clone(),
                password_file: None,
                email_verified: u.email_verified,
                superuser: u.superuser,
            },
        );
    }

    for o in &orgs {
        let members = org_users
            .iter()
            .filter(|ou| ou.organization == o.id)
            .filter_map(|ou| {
                Some(StateOrgMemberEntry {
                    user: username.get(&ou.user)?.clone(),
                    role: role_name.get(&ou.role)?.clone(),
                })
            })
            .collect();
        config.organizations.insert(
            o.name.clone(),
            StateOrganization {
                name: o.name.clone(),
                display_name: o.display_name.clone(),
                id: Some(o.id.to_string()),
                description: opt(&o.description),
                private_key_file: String::new(),
                public: o.public,
                hide_build_requests: o.hide_build_requests,
                github_installation_id: o.github_installation_id,
                created_by: name_or_blank(&username, o.created_by),
                members,
            },
        );
    }

    for p in &projects {
        if p.managed && p.name == "build-request" {
            continue;
        }
        let project_triggers: Vec<StateTrigger> = triggers
            .iter()
            .filter(|t| t.project == p.id)
            .filter_map(|t| export_trigger(t, &integration_name))
            .collect();
        let project_actions = actions
            .iter()
            .filter(|a| a.project == p.id)
            .filter_map(|a| export_action(a, &integration_name))
            .collect();
        let flake_input_overrides = overrides
            .iter()
            .filter(|o| o.project == p.id)
            .map(|o| {
                (
                    o.input_name.clone(),
                    StateFlakeInputOverride {
                        url: o.url.clone(),
                        keep_url: o.url.is_none(),
                    },
                )
            })
            .collect();
        config.projects.insert(
            p.name.clone(),
            StateProject {
                name: p.name.clone(),
                organization: name_or_blank(&org_name, p.organization),
                display_name: p.display_name.clone(),
                description: opt(&p.description),
                repository: p.repository.clone(),
                wildcard: p.wildcard.clone(),
                active: p.active,
                created_by: name_or_blank(&username, p.created_by),
                keep_evaluations: p.keep_evaluations,
                triggers: (!project_triggers.is_empty()).then_some(project_triggers),
                concurrency: ConcurrencyPolicy::try_from(p.concurrency)
                    .unwrap_or(ConcurrencyPolicy::SoftAbort),
                sign_cache: p.sign_cache,
                flake_input_overrides,
                actions: project_actions,
            },
        );
    }

    for c in &caches {
        let organizations = org_caches
            .iter()
            .filter(|oc| oc.cache == c.id)
            .filter_map(|oc| org_name.get(&oc.organization).cloned())
            .collect();
        let cache_upstreams = upstreams
            .iter()
            .filter(|u| u.cache == c.id)
            .filter_map(|u| export_upstream(u, &cache_name))
            .collect();
        let roles = cache_roles
            .iter()
            .filter(|r| r.cache == Some(c.id) && !is_builtin_cache_role(r.id))
            .map(|r| StateCacheRoleEntry {
                name: r.name.clone(),
                permissions: cache_mask_to_vec(r.permission)
                    .into_iter()
                    .map(|p| p.as_wire_name().to_string())
                    .collect(),
            })
            .collect();
        let members = cache_users
            .iter()
            .filter(|cu| cu.cache == c.id)
            .filter_map(|cu| {
                Some(StateCacheMemberEntry {
                    user: username.get(&cu.user)?.clone(),
                    role: cache_role_name.get(&cu.role)?.clone(),
                })
            })
            .collect();
        config.caches.insert(
            c.name.clone(),
            StateCache {
                name: c.name.clone(),
                display_name: c.display_name.clone(),
                description: opt(&c.description),
                active: c.active,
                priority: c.priority,
                local_priority: c.local_priority,
                max_storage_gb: c.max_storage_gb,
                signing_key_file: String::new(),
                organizations,
                upstreams: cache_upstreams,
                public: c.public,
                created_by: name_or_blank(&username, c.created_by),
                roles,
                members,
            },
        );
    }

    for r in &roles {
        let Some(org_id) = r.organization else {
            continue;
        };
        if is_builtin_role(r.id) {
            continue;
        }
        config.roles.insert(
            r.name.clone(),
            StateRole {
                name: r.name.clone(),
                organization: name_or_blank(&org_name, org_id),
                permissions: mask_to_vec(r.permission)
                    .into_iter()
                    .map(|p| p.as_wire_name().to_string())
                    .collect(),
                oidc_group: Vec::new(),
            },
        );
    }

    for k in &api_keys {
        if k.revoked_at.is_some() {
            continue;
        }
        config.api_keys.insert(
            k.name.clone(),
            StateApiKey {
                name: k.name.clone(),
                key_file: String::new(),
                owned_by: name_or_blank(&username, k.owned_by),
                permissions: mask_to_vec(k.permission)
                    .into_iter()
                    .map(|p| p.as_wire_name().to_string())
                    .collect(),
                organization: k.organization.and_then(|id| org_name.get(&id).cloned()),
            },
        );
    }

    // One `worker_registration` row exists per (worker_id, org); fold them back
    // into a single StateWorker carrying the list of orgs.
    let mut worker_orgs: HashMap<String, Vec<String>> = HashMap::new();
    for reg in &registrations {
        if let Some(org) = org_name.get(&reg.peer_id) {
            worker_orgs
                .entry(reg.worker_id.clone())
                .or_default()
                .push(org.clone());
        }
    }
    let mut seen_worker: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for reg in &registrations {
        if !seen_worker.insert(reg.worker_id.as_str()) {
            continue;
        }
        config.workers.insert(
            reg.worker_id.clone(),
            StateWorker {
                worker_id: reg.worker_id.clone(),
                url: reg.url.clone(),
                organizations: worker_orgs.remove(&reg.worker_id).unwrap_or_default(),
                token_file: String::new(),
                display_name: reg.display_name.clone(),
                created_by: reg
                    .created_by
                    .and_then(|id| username.get(&id).cloned())
                    .unwrap_or_default(),
                enable_fetch: reg.enable_fetch,
                enable_eval: reg.enable_eval,
                enable_build: reg.enable_build,
            },
        );
    }

    for i in &integrations {
        let (Ok(kind), Ok(forge)) = (
            IntegrationKind::try_from(i.kind),
            ForgeType::try_from(i.forge_type),
        ) else {
            continue;
        };
        // GitHub integration rows are server-managed and cannot be hand-authored.
        if forge == ForgeType::GitHub {
            continue;
        }

        let forge_type = forge.as_path_segment();
        config.integrations.insert(
            i.name.clone(),
            StateIntegration {
                name: i.name.clone(),
                display_name: Some(i.display_name.clone()),
                organization: name_or_blank(&org_name, i.organization),
                kind: match kind {
                    IntegrationKind::Inbound => "inbound",
                    IntegrationKind::Outbound => "outbound",
                }
                .to_string(),
                forge_type: forge_type.to_string(),
                secret_file: None,
                endpoint_url: i.endpoint_url.clone(),
                access_token_file: None,
                created_by: name_or_blank(&username, i.created_by),
            },
        );
    }

    Ok(config)
}

fn export_trigger(
    t: &gradient_entity::project_trigger::Model,
    integration_name: &HashMap<IntegrationId, String>,
) -> Option<StateTrigger> {
    let cfg = TriggerConfig::parse_row(t.trigger_type, &t.config).ok()?;
    let (trigger_type, integration, config) = match cfg {
        TriggerConfig::Polling {
            interval_secs,
            branch,
        } => {
            let mut c = serde_json::Map::new();
            c.insert("interval_secs".into(), interval_secs.into());
            if let Some(b) = branch {
                c.insert("branch".into(), b.into());
            }
            (TriggerType::Polling, None, c)
        }
        TriggerConfig::ReporterPush {
            integration_id,
            branches,
            tags,
            releases_only,
        } => {
            let mut c = serde_json::Map::new();
            c.insert("branches".into(), branches.into());
            c.insert("tags".into(), tags.into());
            c.insert("releases_only".into(), releases_only.into());
            (
                TriggerType::ReporterPush,
                integration_name.get(&integration_id).cloned(),
                c,
            )
        }
        TriggerConfig::ReporterPullRequest {
            integration_id,
            branches,
            actions,
            require_approval,
        } => {
            let mut c = serde_json::Map::new();
            c.insert("branches".into(), branches.into());
            c.insert("actions".into(), actions.into());
            c.insert("require_approval".into(), require_approval.into());
            (
                TriggerType::ReporterPullRequest,
                integration_name.get(&integration_id).cloned(),
                c,
            )
        }
        TriggerConfig::Time { cron } => {
            let mut c = serde_json::Map::new();
            c.insert("cron".into(), cron.into());
            (TriggerType::Time, None, c)
        }
    };
    Some(StateTrigger {
        trigger_type,
        integration,
        config: serde_json::Value::Object(config),
        active: t.active,
    })
}

fn export_action(
    a: &gradient_entity::project_action::Model,
    integration_name: &HashMap<IntegrationId, String>,
) -> Option<super::StateAction> {
    let cfg: ActionConfig = serde_json::from_value(a.config.clone()).ok()?;
    let events: Vec<String> = serde_json::from_value(a.events.clone()).unwrap_or_default();
    let (action_type, config) = match cfg {
        // The stored web-request token is encrypted and unrecoverable, so the
        // `token_file` path is dropped - re-add it in nix if the hook needs auth.
        ActionConfig::SendMail {
            recipients,
            subject_template,
        } => {
            let mut c = serde_json::Map::new();
            c.insert("recipients".into(), recipients.into());
            if let Some(s) = subject_template {
                c.insert("subject_template".into(), s.into());
            }
            (ActionType::SendMail, c)
        }
        ActionConfig::SendWebRequest { url, .. } => {
            let mut c = serde_json::Map::new();
            c.insert("url".into(), url.into());
            (ActionType::SendWebRequest, c)
        }
        ActionConfig::ForgeStatusReport { integration_id } => {
            let mut c = serde_json::Map::new();
            c.insert(
                "integration".into(),
                integration_name.get(&integration_id).cloned()?.into(),
            );
            (ActionType::ForgeStatusReport, c)
        }
    };
    Some(super::StateAction {
        name: a.name.clone(),
        action_type: match action_type {
            ActionType::SendMail => "send_mail",
            ActionType::SendWebRequest => "send_web_request",
            ActionType::ForgeStatusReport => "forge_status_report",
        }
        .to_string(),
        active: a.active,
        events,
        config: serde_json::Value::Object(config),
    })
}

fn export_upstream(
    u: &gradient_entity::cache_upstream::Model,
    cache_name: &HashMap<CacheId, String>,
) -> Option<StateUpstream> {
    match u.kind {
        CacheUpstreamKind::Internal => Some(StateUpstream::Internal {
            cache_name: cache_name.get(&u.upstream_cache?)?.clone(),
            display_name: Some(u.display_name.clone()),
            mode: u.mode.clone(),
        }),
        CacheUpstreamKind::Http => Some(StateUpstream::External {
            display_name: u.display_name.clone(),
            url: u.url.clone()?,
            public_key: u.public_key.clone()?,
        }),
        // GradientProto upstreams have no `state` representation yet.
        CacheUpstreamKind::GradientProto => None,
    }
}

fn id_name_map<K: Eq + std::hash::Hash>(
    pairs: impl Iterator<Item = (K, String)>,
) -> HashMap<K, String> {
    pairs.collect()
}

fn name_or_blank<K: Eq + std::hash::Hash>(map: &HashMap<K, String>, id: K) -> String {
    map.get(&id).cloned().unwrap_or_default()
}

/// `""` (the DB's not-null default for optional text columns) maps back to a
/// `null` description in the declarative shape.
fn opt(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Serialize the config and null out every credential-file field so secrets
/// never leak. The resulting [`serde_json::Value`] drives both the JSON and the
/// Nix response.
pub fn redact(config: &StateConfiguration) -> serde_json::Value {
    let mut value = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
    redact_value(&mut value);
    value
}

fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if SECRET_KEYS.contains(&k.as_str()) {
                    *v = serde_json::Value::Null;
                } else {
                    redact_value(v);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_value),
        _ => {}
    }
}

/// Render a redacted state [`serde_json::Value`] as a Nix expression assignable
/// to `services.gradient.state`. A header comment flags the redacted secrets.
pub fn to_nix(value: &serde_json::Value) -> String {
    let mut out = String::from(
        "# Generated by `GET /admin/state`. Secret `*_file` fields are null and\n\
         # must be filled in with the credential paths on your host.\n",
    );
    render_nix(value, 0, &mut out);
    out.push('\n');
    out
}

fn render_nix(value: &serde_json::Value, indent: usize, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => out.push_str(&n.to_string()),
        serde_json::Value::String(s) => out.push_str(&nix_string(s)),
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[ ]");
                return;
            }
            out.push_str("[\n");
            let pad = "  ".repeat(indent + 1);
            for item in items {
                out.push_str(&pad);
                render_nix(item, indent + 1, out);
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{ }");
                return;
            }
            out.push_str("{\n");
            let pad = "  ".repeat(indent + 1);
            for (k, v) in map {
                out.push_str(&pad);
                out.push_str(&nix_key(k));
                out.push_str(" = ");
                render_nix(v, indent + 1, out);
                out.push_str(";\n");
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
    }
}

/// Bare identifier when the key is a simple nix name, quoted otherwise (e.g.
/// worker_id UUIDs, which start with a digit).
fn nix_key(key: &str) -> String {
    let simple = !key.is_empty()
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if simple {
        key.to_string()
    } else {
        nix_string(key)
    }
}

fn nix_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '$' => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_nulls_secret_files_at_any_depth() {
        let mut v = json!({
            "users": { "alice": { "username": "alice", "password_file": "/etc/pw" } },
            "caches": { "main": { "signing_key_file": "/etc/key", "name": "main" } },
            "workers": { "w1": { "token_file": "/etc/tok" } }
        });
        redact_value(&mut v);
        assert!(v["users"]["alice"]["password_file"].is_null());
        assert!(v["caches"]["main"]["signing_key_file"].is_null());
        assert!(v["workers"]["w1"]["token_file"].is_null());
        assert_eq!(v["users"]["alice"]["username"], "alice");
    }

    #[test]
    fn nix_string_escapes_specials() {
        assert_eq!(nix_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(nix_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(nix_string("a${b}"), "\"a\\${b}\"");
        assert_eq!(nix_string("line\n"), "\"line\\n\"");
    }

    #[test]
    fn nix_key_quotes_non_identifiers() {
        assert_eq!(nix_key("alice"), "alice");
        assert_eq!(nix_key("build-request"), "build-request");
        assert_eq!(nix_key("123e4567-uuid"), "\"123e4567-uuid\"");
        assert_eq!(nix_key("with space"), "\"with space\"");
    }

    #[test]
    fn to_nix_renders_nested_structure() {
        let v = json!({
            "users": {
                "alice": {
                    "username": "alice",
                    "superuser": true,
                    "password_file": null,
                    "tags": ["a", "b"]
                }
            },
            "empty": {}
        });
        let nix = to_nix(&v);
        assert!(nix.contains("users = {"));
        assert!(nix.contains("alice = {"));
        assert!(nix.contains("superuser = true;"));
        assert!(nix.contains("password_file = null;"));
        assert!(nix.contains("tags = [\n"));
        assert!(nix.contains("empty = { };"));
        assert!(nix.starts_with("# Generated by"));
    }

    #[test]
    fn to_nix_renders_empty_state() {
        let v = json!({});
        let nix = to_nix(&v);
        assert!(nix.contains("{ }"));
    }
}
