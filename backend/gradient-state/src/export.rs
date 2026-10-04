/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    StateApiKey, StateCache, StateCacheMemberEntry, StateCacheRoleEntry, StateCacheTeam,
    StateConfiguration, StateFlakeInputOverride, StateIntegration, StateNewProjectGrant,
    StateProject, StateProjectMemberEntry, StateProjectTeam, StateRole, StateTask, StateTeam,
    StateTeamMember, StateTrigger, StateUpstream, StateUser, StateWorker,
};
use gradient_ci::IntegrationKind;
use gradient_db::permissions::{
    cache_mask_to_vec, is_builtin_cache_role, is_builtin_role, mask_to_vec,
};
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::ids::*;
use gradient_entity::team_user::{TeamMemberSource, TeamRole};
use gradient_types::actions::{ActionConfig, ActionType};
use gradient_types::triggers::{TriggerConfig, TriggerType};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashMap;

const SECRET_KEYS: &[&str] = &[
    "password_file",
    "private_key_file",
    "signing_key_file",
    "token_file",
    "key_file",
    "secret_file",
    "access_token_file",
    "webhook_url_file",
];

/// The snapshot is covering the live system, not only state-managed rows. Rows an operator cannot
/// hand-author are excluded. These are the `build-request` task, server-managed GitHub integration
/// rows, Gradient.CI connections, members added through an SSO group and the built-in `Admin`/`Write`/`View` roles.
pub async fn export_state<C: ConnectionTrait>(db: &C) -> Result<StateConfiguration, DbErr> {
    let users = gradient_entity::user::Entity::find().all(db).await?;
    let projects = gradient_entity::project::Entity::find().all(db).await?;
    let tasks = gradient_entity::task::Entity::find().all(db).await?;
    let caches = gradient_entity::cache::Entity::find().all(db).await?;
    let roles = gradient_entity::role::Entity::find().all(db).await?;
    let cache_roles = gradient_entity::cache_role::Entity::find().all(db).await?;
    let api_keys = gradient_entity::api::Entity::find().all(db).await?;
    let registrations = gradient_entity::worker_registration::Entity::find()
        .filter(gradient_entity::worker_registration::Column::GradientCi.eq(false))
        .all(db)
        .await?;
    let teams = gradient_entity::team::Entity::find().all(db).await?;
    let team_users = gradient_entity::team_user::Entity::find()
        .filter(gradient_entity::team_user::Column::Source.ne(TeamMemberSource::Group))
        .all(db)
        .await?;
    let team_projects = gradient_entity::team_project::Entity::find()
        .all(db)
        .await?;
    let team_caches = gradient_entity::team_cache::Entity::find().all(db).await?;
    let team_workers = gradient_entity::team_worker::Entity::find()
        .filter(gradient_entity::team_worker::Column::GradientCi.eq(false))
        .all(db)
        .await?;
    let integrations = gradient_entity::integration::Entity::find().all(db).await?;
    let project_users = gradient_entity::project_user::Entity::find()
        .all(db)
        .await?;
    let cache_users = gradient_entity::cache_user::Entity::find().all(db).await?;
    let project_caches = gradient_entity::project_cache::Entity::find()
        .all(db)
        .await?;
    let all_upstream_caches = gradient_entity::cache_upstream::Entity::find()
        .all(db)
        .await?;
    let triggers = gradient_entity::task_trigger::Entity::find()
        .all(db)
        .await?;
    let actions = gradient_entity::task_action::Entity::find().all(db).await?;
    let overrides = gradient_entity::task_flake_input_override::Entity::find()
        .all(db)
        .await?;
    let github_installs = gradient_entity::github_installation::Entity::find()
        .all(db)
        .await?;

    let username = id_name_map(users.iter().map(|u| (u.id, u.username.clone())));
    let project_name = id_name_map(projects.iter().map(|o| (o.id, o.name.clone())));
    let cache_name = id_name_map(caches.iter().map(|c| (c.id, c.name.clone())));
    let role_name = id_name_map(roles.iter().map(|r| (r.id, r.name.clone())));
    let cache_role_name = id_name_map(cache_roles.iter().map(|r| (r.id, r.name.clone())));
    let integration_name = id_name_map(integrations.iter().map(|i| (i.id, i.name.clone())));
    let github_install_by_id: HashMap<_, _> = github_installs.iter().map(|g| (g.id, g)).collect();
    let team_name = id_name_map(teams.iter().map(|t| (t.id, t.name.clone())));

    let mut config = StateConfiguration {
        users: HashMap::new(),
        projects: HashMap::new(),
        tasks: HashMap::new(),
        caches: HashMap::new(),
        roles: HashMap::new(),
        api_keys: HashMap::new(),
        workers: HashMap::new(),
        integrations: HashMap::new(),
        teams: HashMap::new(),
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

    for o in &projects {
        let members = project_users
            .iter()
            .filter(|ou| ou.project == o.id)
            .filter_map(|ou| {
                Some(StateProjectMemberEntry {
                    user: username.get(&ou.user)?.clone(),
                    role: role_name.get(&ou.role)?.clone(),
                })
            })
            .collect();
        let teams = team_projects
            .iter()
            .filter(|g| g.project == o.id)
            .filter_map(|g| {
                Some(StateProjectTeam {
                    team: team_name.get(&g.team)?.clone(),
                    role: g.role.and_then(|role| role_name.get(&role).cloned()),
                    users: g.includes_users,
                    workers: g.includes_workers,
                })
            })
            .collect();
        config.projects.insert(
            o.name.clone(),
            StateProject {
                name: o.name.clone(),
                display_name: o.display_name.clone(),
                id: Some(o.id.to_string()),
                description: opt(&o.description),
                private_key_file: String::new(),
                public: o.public,
                hide_build_requests: o.hide_build_requests,
                created_by: name_or_blank(&username, o.created_by),
                members,
                teams,
            },
        );
    }

    for p in &tasks {
        if p.managed && p.name == "build-request" {
            continue;
        }
        let task_triggers: Vec<StateTrigger> = triggers
            .iter()
            .filter(|t| t.task == p.id)
            .filter_map(|t| export_trigger(t, &integration_name))
            .collect();
        let task_actions = actions
            .iter()
            .filter(|a| a.task == p.id)
            .filter_map(|a| export_action(a, &integration_name))
            .collect();
        let flake_input_overrides = overrides
            .iter()
            .filter(|o| o.task == p.id)
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
        config.tasks.insert(
            p.name.clone(),
            StateTask {
                name: p.name.clone(),
                project: name_or_blank(&project_name, p.project),
                display_name: p.display_name.clone(),
                description: opt(&p.description),
                repository: p.repository.clone(),
                wildcard: p.wildcard.clone(),
                active: p.active,
                created_by: name_or_blank(&username, p.created_by),
                keep_evaluations: p.keep_evaluations,
                triggers: (!task_triggers.is_empty()).then_some(task_triggers),
                concurrency: p.concurrency,
                sign_cache: p.sign_cache,
                wait_for_workers: p.wait_for_workers,
                flake_input_overrides,
                actions: task_actions,
            },
        );
    }

    for c in &caches {
        let projects = project_caches
            .iter()
            .filter(|oc| oc.cache == c.id)
            .filter_map(|oc| project_name.get(&oc.project).cloned())
            .collect();
        let upstream_caches = all_upstream_caches
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
        let teams = team_caches
            .iter()
            .filter(|g| g.cache == c.id)
            .filter_map(|g| {
                Some(StateCacheTeam {
                    team: team_name.get(&g.team)?.clone(),
                    role: cache_role_name.get(&g.role)?.clone(),
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
                projects,
                upstream_caches,
                public: c.public,
                created_by: name_or_blank(&username, c.created_by),
                roles,
                members,
                teams,
            },
        );
    }

    for t in &teams {
        config.teams.insert(
            t.name.clone(),
            export_team(t, &team_users, &username, &role_name),
        );
    }

    for r in &roles {
        let Some(project_id) = r.project else {
            continue;
        };
        if is_builtin_role(r.id) {
            continue;
        }
        config.roles.insert(
            r.name.clone(),
            StateRole {
                name: r.name.clone(),
                project: name_or_blank(&project_name, project_id),
                permissions: mask_to_vec(r.permission)
                    .into_iter()
                    .map(|p| p.as_wire_name().to_string())
                    .collect(),
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
                project: k.project.and_then(|id| project_name.get(&id).cloned()),
            },
        );
    }

    let mut worker_projects: HashMap<String, Vec<String>> = HashMap::new();
    for reg in &registrations {
        if let Some(project) = project_name.get(&reg.peer_id) {
            worker_projects
                .entry(reg.worker_id.clone())
                .or_default()
                .push(project.clone());
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
                projects: worker_projects.remove(&reg.worker_id).unwrap_or_default(),
                token_file: String::new(),
                display_name: reg.display_name.clone(),
                created_by: reg.created_by.and_then(|id| username.get(&id).cloned()),
                enable_fetch: reg.enable_fetch,
                enable_eval: reg.enable_eval,
                enable_build: reg.enable_build,
                team: None,
                enabled: true,
            },
        );
    }

    for worker in &team_workers {
        config.workers.insert(
            worker.worker_id.clone(),
            export_team_worker(worker, &team_name, &username),
        );
    }

    for i in &integrations {
        let install = i
            .github_installation
            .and_then(|fk| github_install_by_id.get(&fk));

        config.integrations.insert(
            i.name.clone(),
            StateIntegration {
                name: i.name.clone(),
                display_name: Some(i.display_name.clone()),
                project: name_or_blank(&project_name, i.project),
                kind: match i.kind {
                    IntegrationKind::Inbound => "inbound",
                    IntegrationKind::Outbound => "outbound",
                }
                .to_string(),
                git_host_type: i.git_host_type.as_path_segment().to_string(),
                secret_file: None,
                endpoint_url: i.endpoint_url.clone(),
                access_token_file: None,
                installation_id: install.map(|g| g.installation_id),
                account_login: install.and_then(|g| g.account_login.clone()),
                created_by: name_or_blank(&username, i.created_by),
            },
        );
    }

    Ok(config)
}

fn export_team(
    team: &gradient_entity::team::Model,
    members: &[gradient_entity::team_user::Model],
    username: &HashMap<UserId, String>,
    role_name: &HashMap<RoleId, String>,
) -> StateTeam {
    StateTeam {
        name: team.name.clone(),
        display_name: team.display_name.clone(),
        members: members
            .iter()
            .filter(|m| m.team == team.id)
            .filter_map(|m| {
                Some(StateTeamMember {
                    user: username.get(&m.user)?.clone(),
                    role: match m.role {
                        TeamRole::Admin => "Admin",
                        TeamRole::Member => "Member",
                    }
                    .to_string(),
                })
            })
            .collect(),
        oidc_group: team.oidc_group.clone(),
        scim_group: team.scim_group.clone(),
        new_projects: StateNewProjectGrant {
            users: team.new_project_users,
            workers: team.new_project_workers,
            role: team
                .new_project_role
                .and_then(|role| role_name.get(&role).cloned()),
        },
    }
}

fn export_team_worker(
    worker: &gradient_entity::team_worker::Model,
    team_name: &HashMap<TeamId, String>,
    username: &HashMap<UserId, String>,
) -> StateWorker {
    StateWorker {
        worker_id: worker.worker_id.clone(),
        url: worker.url.clone(),
        projects: Vec::new(),
        team: team_name.get(&worker.team).cloned(),
        token_file: String::new(),
        display_name: worker.display_name.clone(),
        created_by: worker.created_by.and_then(|id| username.get(&id).cloned()),
        enable_fetch: worker.enable_fetch,
        enable_eval: worker.enable_eval,
        enable_build: worker.enable_build,
        enabled: worker.active,
    }
}

fn export_trigger(
    t: &gradient_entity::task_trigger::Model,
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

fn enum_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn export_action(
    a: &gradient_entity::task_action::Model,
    integration_name: &HashMap<IntegrationId, String>,
) -> Option<super::StateAction> {
    let cfg: ActionConfig = serde_json::from_value(a.config.clone()).ok()?;
    let events: Vec<String> = serde_json::from_value(a.events.clone()).unwrap_or_default();
    let (action_type, config) = match cfg {
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
        ActionConfig::GitHostStatusReport { integration_id } => {
            let mut c = serde_json::Map::new();
            c.insert(
                "integration".into(),
                integration_name.get(&integration_id).cloned()?.into(),
            );
            (ActionType::GitHostStatusReport, c)
        }
        ActionConfig::OpenPr {
            integration_id,
            generator,
            granularity,
            verify_gate,
            branch_pattern,
            title_template,
            body_template,
            update_existing,
        } => {
            let mut c = serde_json::Map::new();
            c.insert(
                "integration".into(),
                integration_name.get(&integration_id).cloned()?.into(),
            );
            c.insert("generator".into(), enum_str(&generator).into());
            c.insert("granularity".into(), enum_str(&granularity).into());
            c.insert("verify_gate".into(), enum_str(&verify_gate).into());
            c.insert("branch_pattern".into(), branch_pattern.into());
            c.insert(
                "title_template".into(),
                title_template
                    .map(Into::into)
                    .unwrap_or(serde_json::Value::Null),
            );
            c.insert(
                "body_template".into(),
                body_template
                    .map(Into::into)
                    .unwrap_or(serde_json::Value::Null),
            );
            c.insert("update_existing".into(), update_existing.into());
            (ActionType::OpenPr, c)
        }
        ActionConfig::SendMatrixMessage {
            homeserver,
            room_id,
            ..
        } => {
            let mut c = serde_json::Map::new();
            c.insert("homeserver".into(), homeserver.into());
            c.insert("room_id".into(), room_id.into());
            (ActionType::SendMatrixMessage, c)
        }
        ActionConfig::SendSlackMessage { .. } => {
            (ActionType::SendSlackMessage, serde_json::Map::new())
        }
    };
    Some(super::StateAction {
        name: a.name.clone(),
        action_type: action_type.as_str().to_string(),
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
            active: u.active,
        }),
        CacheUpstreamKind::Http => Some(StateUpstream::External {
            display_name: u.display_name.clone(),
            url: u.url.clone()?,
            public_key: u.public_key.clone()?,
            active: u.active,
        }),
        // GradientProto upstream caches have no `state` representation yet.
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

fn opt(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

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
    fn export_team_worker_names_its_team() {
        let team = TeamId::now_v7();
        let user = UserId::now_v7();
        let worker = gradient_entity::team_worker::Model {
            team,
            worker_id: "tw-1".to_string(),
            token_hash: "hash".to_string(),
            url: Some("https://tw.example".to_string()),
            display_name: "Team Worker".to_string(),
            enable_fetch: true,
            enable_eval: false,
            enable_build: true,
            active: true,
            created_by: Some(user),
            ..Default::default()
        };
        let team_name = HashMap::from([(team, "platform".to_string())]);
        let username = HashMap::from([(user, "alice".to_string())]);

        let sw = export_team_worker(&worker, &team_name, &username);

        assert_eq!(sw.team, Some("platform".to_string()));
        assert!(sw.projects.is_empty());
        assert!(sw.enabled);
        assert!(!sw.enable_eval);
        assert_eq!(sw.created_by, Some("alice".to_string()));
        assert!(sw.token_file.is_empty());
    }

    #[test]
    fn disabled_upstream_survives_export_round_trip() {
        let upstream = gradient_entity::cache_upstream::Model {
            kind: CacheUpstreamKind::Http,
            url: Some("https://cache.nixos.org".into()),
            public_key: Some("cache.nixos.org-1:abc".into()),
            active: false,
            ..Default::default()
        };

        let exported = export_upstream(&upstream, &HashMap::new()).unwrap();
        let reread: StateUpstream =
            serde_json::from_value(serde_json::to_value(&exported).unwrap()).unwrap();
        assert!(matches!(
            reread,
            StateUpstream::External { active: false, .. }
        ));

        let declared: StateUpstream = serde_json::from_value(json!({
            "type": "external",
            "display_name": "nixos",
            "url": "https://cache.nixos.org",
            "public_key": "cache.nixos.org-1:abc",
        }))
        .unwrap();
        assert!(matches!(
            declared,
            StateUpstream::External { active: true, .. }
        ));
    }

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
