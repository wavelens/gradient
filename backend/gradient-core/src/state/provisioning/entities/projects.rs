/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{
    inbound_integrations_by_name, lookup_id, outbound_integrations_by_name, read_credential,
};
use crate::state::config::*;
use crate::types::triggers::TriggerConfig;
use crate::types::*;
use anyhow::{Context, Result};
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

impl<'a> StateApplicator<'a> {
    // ── apply_projects ────────────────────────────────────────────────────────

    pub(crate) async fn apply_projects(
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
                let outbound_integrations_by_name =
                    outbound_integrations_by_name(self.db, org_id).await?;

                apply_project_triggers(
                    self.db,
                    &project_row,
                    triggers,
                    &inbound_integrations_by_name,
                    &outbound_integrations_by_name,
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

    pub(crate) async fn apply_project_actions(
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

    pub(crate) async fn apply_flake_input_overrides(
        &self,
        project_id: ProjectId,
        desired: &HashMap<String, StateFlakeInputOverride>,
    ) -> Result<(), DynError> {
        use gradient_entity::project_flake_input_override as pfio;

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
                        id: Set(gradient_entity::ids::FlakeInputOverrideId::now_v7()),
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
}

pub(crate) async fn apply_project_triggers<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    desired: &[StateTrigger],
    inbound_by_name: &HashMap<String, IntegrationId>,
    outbound_by_name: &HashMap<String, IntegrationId>,
) -> anyhow::Result<()> {
    if desired.is_empty() {
        anyhow::bail!("project '{}' must have at least one trigger", project.name);
    }

    let mut desired_by_key: HashMap<String, (TriggerConfig, bool)> = HashMap::new();
    for t in desired {
        let cfg = build_trigger_config(t, inbound_by_name, outbound_by_name)?;
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

pub(crate) fn build_trigger_config(
    t: &StateTrigger,
    inbound: &HashMap<String, IntegrationId>,
    outbound: &HashMap<String, IntegrationId>,
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
            let id = match inbound.get(name) {
                Some(id) => *id,
                None if outbound.contains_key(name) => anyhow::bail!(
                    "integration '{name}' is configured as `outbound`, but reporter triggers \
                     require an `inbound` integration to receive forge webhooks. Declare an \
                     `inbound` integration and reference outbound integrations via the project's \
                     `outbound_integration` or a `forge_status_report` action."
                ),
                None => anyhow::bail!("unknown integration: {name}"),
            };
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

pub(crate) fn trigger_key(cfg: &TriggerConfig) -> String {
    let json = cfg.to_db_json();
    let canonical = serde_json::to_string(&json).unwrap_or_default();
    format!("{}|{}", i16::from(cfg.trigger_type()), canonical)
}

/// Build a stored `ActionConfig` from a declared `StateAction`. Tokens for
/// `send_web_request` are loaded from the systemd credential file
/// `gradient_action_${name}_token` and encrypted with the server's crypt key
/// before storage, matching the REST `create_action` path.
pub(crate) fn build_action_config(
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
            crate::http_validation::validate_webhook_url(&url)
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

#[cfg(test)]
mod trigger_helper_tests {
    use super::{build_trigger_config, trigger_key};
    use crate::state::StateTrigger;
    use crate::types::IntegrationId;
    use crate::types::triggers::{TriggerConfig, TriggerType};
    use std::collections::HashMap;

    pub(crate) fn polling_trigger(interval_secs: u64) -> StateTrigger {
        StateTrigger {
            trigger_type: TriggerType::Polling,
            integration: None,
            config: serde_json::json!({ "interval_secs": interval_secs }),
            active: true,
        }
    }

    pub(crate) fn empty_integrations() -> HashMap<String, IntegrationId> {
        HashMap::new()
    }

    #[test]
    fn build_polling_trigger() {
        let t = polling_trigger(60);
        let cfg = build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap();
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
        let cfg = build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap();
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
        let err =
            build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap_err();
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
        let cfg = build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap();
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
        let err =
            build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap_err();
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
        let err =
            build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap_err();
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
        let err =
            build_trigger_config(&t, &empty_integrations(), &empty_integrations()).unwrap_err();
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
        let cfg = build_trigger_config(&t, &integrations, &empty_integrations()).unwrap();
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

    #[test]
    fn build_reporter_pr_rejects_outbound_integration_with_kind_aware_error() {
        let mut outbound = HashMap::new();
        outbound.insert("forgejo-status-reports".into(), IntegrationId::nil());

        let t = StateTrigger {
            trigger_type: TriggerType::ReporterPullRequest,
            integration: Some("forgejo-status-reports".into()),
            config: serde_json::json!({}),
            active: true,
        };

        let err = build_trigger_config(&t, &empty_integrations(), &outbound).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("forgejo-status-reports"),
            "error names the integration: {msg}"
        );
        assert!(
            msg.contains("outbound") && msg.contains("inbound"),
            "error explains the inbound/outbound kind mismatch: {msg}"
        );
    }

    // TODO: integration test for apply_project_triggers full DB round-trip (T30 smoke)
}

#[cfg(test)]
mod action_helper_tests {
    use super::*;
    use uuid::Uuid;

    pub(crate) fn key() -> Vec<u8> {
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
