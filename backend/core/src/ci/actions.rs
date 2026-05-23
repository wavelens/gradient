/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Project Actions dispatch and execution.

use crate::ci::integration_lookup::{ForgeType, IntegrationKind};
use crate::ci::reporter::{
    CiReport, CiReporter, CiStatus, GiteaReporter, GithubAppReporter, GithubReporter,
    GitlabReporter,
};
use crate::ci::webhook::decrypt_webhook_secret;
use crate::types::input::load_secret_bytes;
use crate::types::{
    ActionConfig, ActionType, AProjectActionDelivery, CIntegration, CProjectAction, EIntegration,
    EOrganization, EProjectAction, IntegrationId, MProjectAction, ProjectActionDeliveryId,
    ProjectId, ServerState,
};
use anyhow::{Context, Result, anyhow};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value as JsonValue;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, warn};

pub fn encrypt_action_secret(plaintext: &str, crypt_key: &[u8]) -> Result<String> {
    super::action_crypto::encrypt(plaintext, crypt_key)
}

pub fn decrypt_action_secret(ciphertext: &str, crypt_key: &[u8]) -> Result<String> {
    super::action_crypto::decrypt(ciphertext, crypt_key)
}

/// Forge-status reports are limited to a fixed lifecycle vocabulary so the
/// matched event always maps to a single CI state.
pub const FORGE_STATUS_EVENTS: &[&str] = &["build.started", "build.completed", "build.failed"];

/// Truncate persisted request / response payloads so a chatty upstream does
/// not balloon the delivery table.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

pub fn matches_event(action: &MProjectAction, event: &str) -> bool {
    if action.action_type == ActionType::ForgeStatusReport.to_i16() {
        return FORGE_STATUS_EVENTS.contains(&event);
    }
    action
        .events
        .as_array()
        .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(event)))
}

pub fn forge_status_for_event(event: &str) -> Option<CiStatus> {
    match event {
        "build.started" => Some(CiStatus::Running),
        "build.completed" => Some(CiStatus::Success),
        "build.failed" => Some(CiStatus::Failure),
        _ => None,
    }
}

pub async fn dispatch_evaluation_event(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    event: &str,
    payload: JsonValue,
) {
    dispatch_event(state, project_id, event, payload).await;
}

pub async fn dispatch_build_event(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    event: &str,
    payload: JsonValue,
) {
    dispatch_event(state, project_id, event, payload).await;
}

async fn dispatch_event(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    event: &str,
    payload: JsonValue,
) {
    let actions = match EProjectAction::find()
        .filter(CProjectAction::Project.eq(project_id))
        .filter(CProjectAction::Active.eq(true))
        .all(&state.worker_db)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            error!(error = %e, %project_id, "Failed to load project actions");
            return;
        }
    };

    for action in actions {
        if !matches_event(&action, event) {
            continue;
        }
        let state = Arc::clone(state);
        let payload = payload.clone();
        let event = event.to_string();
        tokio::spawn(async move {
            if let Err(e) = execute_action(&state, action, &event, payload).await {
                warn!(error = %e, "Action execution failed");
            }
        });
    }
}

pub struct ExecutorOk {
    pub status_code: Option<i32>,
    pub response_body: Option<String>,
}

pub async fn execute_action(
    state: &Arc<ServerState>,
    action: MProjectAction,
    event: &str,
    payload: JsonValue,
) -> Result<()> {
    let cfg: ActionConfig =
        serde_json::from_value(action.config.clone()).context("decoding action config")?;
    let started = Instant::now();
    let request_body = truncate(
        serde_json::to_string(&payload).unwrap_or_default(),
        MAX_BODY_BYTES,
    );

    let result = match cfg {
        ActionConfig::SendMail {
            recipients,
            subject_template,
        } => {
            execute_send_mail(
                state,
                event,
                &payload,
                &recipients,
                subject_template.as_deref(),
            )
            .await
        }
        ActionConfig::SendWebRequest { url, token } => {
            execute_send_web_request(state, event, &payload, &url, token.as_deref()).await
        }
        ActionConfig::ForgeStatusReport { integration_id } => {
            execute_forge_status_report(state, event, &payload, integration_id).await
        }
    };

    let duration_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let success = result.is_ok();
    let (response_status, response_body, error_message) = match &result {
        Ok(ok) => (ok.status_code, ok.response_body.clone(), None),
        Err(e) => (None, None, Some(format!("{:#}", e))),
    };

    let action_id = action.id;
    let delivery = AProjectActionDelivery {
        id: Set(ProjectActionDeliveryId::now_v7()),
        action_id: Set(action_id),
        event: Set(event.to_string()),
        request_body: Set(request_body),
        response_status: Set(response_status),
        response_body: Set(response_body.map(|s| truncate(s, MAX_BODY_BYTES))),
        error_message: Set(error_message),
        success: Set(success),
        duration_ms: Set(duration_ms),
        delivered_at: Set(crate::types::now()),
    };
    if let Err(e) = delivery.insert(&state.worker_db).await {
        warn!(error = %e, %action_id, "Failed to record action delivery");
    }

    if success {
        let mut am = sea_orm::IntoActiveModel::into_active_model(action);
        am.last_fired_at = Set(Some(crate::types::now()));
        am.updated_at = Set(crate::types::now());
        if let Err(e) = am.update(&state.worker_db).await {
            warn!(error = %e, %action_id, "Failed to update action last_fired_at");
        }
    }

    result.map(|_| ())
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        if let Some((idx, _)) = s.char_indices().take_while(|(i, _)| *i <= max).last() {
            s.truncate(idx);
        } else {
            s.truncate(max);
        }
    }
    s
}

async fn execute_send_mail(
    state: &Arc<ServerState>,
    event: &str,
    payload: &JsonValue,
    recipients: &[String],
    subject_template: Option<&str>,
) -> Result<ExecutorOk> {
    if recipients.is_empty() {
        return Err(anyhow!("send_mail action has no recipients"));
    }
    let subject = render_subject(subject_template, event, payload);
    let body = render_default_body(event, payload);
    let r = state.email.send_action_mail(recipients, &subject, &body).await?;
    Ok(ExecutorOk {
        status_code: Some(r.status_code),
        response_body: Some(r.server_response),
    })
}

async fn execute_send_web_request(
    state: &Arc<ServerState>,
    event: &str,
    payload: &JsonValue,
    url: &str,
    token: Option<&str>,
) -> Result<ExecutorOk> {
    crate::ci::webhook::validate_webhook_url(url)
        .map_err(|e| anyhow!("URL rejected: {}", e))?;
    let body = serde_json::to_string(payload).context("serializing webhook payload")?;
    let mut req = state
        .http
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Gradient-Event", event)
        .body(body);
    if let Some(tok) = token {
        let key = load_secret_bytes(&state.config.secrets.crypt_secret_file)
            .context("loading crypt key")?;
        let decrypted = decrypt_action_secret(tok, key.expose())?;
        req = req.bearer_auth(decrypted);
    }
    let resp = req.send().await.context("HTTP send failed")?;
    let status = resp.status().as_u16() as i32;
    let response_body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(anyhow!("upstream returned HTTP {}: {}", status, truncate(response_body, 256)));
    }
    Ok(ExecutorOk {
        status_code: Some(status),
        response_body: Some(truncate(response_body, MAX_BODY_BYTES)),
    })
}

async fn execute_forge_status_report(
    state: &Arc<ServerState>,
    event: &str,
    payload: &JsonValue,
    integration_id: IntegrationId,
) -> Result<ExecutorOk> {
    let ci_status = forge_status_for_event(event)
        .ok_or_else(|| anyhow!("event '{}' has no forge status mapping", event))?;

    let report = build_ci_report_from_payload(state, payload, ci_status).await?;
    let reporter = build_reporter_for_integration(state, integration_id).await?;
    let new_id = reporter
        .report(&report)
        .await
        .context("forge status report failed")?;
    let body = new_id.map(|id| format!("{{\"check_run_id\":{}}}", id));
    Ok(ExecutorOk {
        status_code: Some(200),
        response_body: body,
    })
}

/// Constructs a `CiReport` from the dispatcher's JSON payload. Required fields
/// (`owner`, `repo`, `sha`, `context`) must be present; emitters that fire
/// forge-status-eligible events are responsible for supplying them.
async fn build_ci_report_from_payload(
    _state: &Arc<ServerState>,
    payload: &JsonValue,
    status: CiStatus,
) -> Result<CiReport> {
    let s = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
    };
    let owner = s("owner").ok_or_else(|| anyhow!("payload missing 'owner'"))?;
    let repo = s("repo").ok_or_else(|| anyhow!("payload missing 'repo'"))?;
    let sha = s("sha").ok_or_else(|| anyhow!("payload missing 'sha'"))?;
    let context = s("context").ok_or_else(|| anyhow!("payload missing 'context'"))?;
    let description = s("description");
    let details_url = s("details_url");
    let existing_check_id = payload.get("check_run_id").and_then(|v| v.as_i64());
    Ok(CiReport {
        owner,
        repo,
        sha,
        context,
        status,
        description,
        details_url,
        existing_check_id,
        requested_actions: Vec::new(),
    })
}

/// Build a `CiReporter` from an integration row referenced by id. Returns a
/// concrete reporter or a hard error so the delivery row reflects the
/// misconfiguration — unlike the project-level resolver which silently falls
/// back to `NoopCiReporter`.
async fn build_reporter_for_integration(
    state: &Arc<ServerState>,
    integration_id: IntegrationId,
) -> Result<Arc<dyn CiReporter>> {
    let integration = EIntegration::find_by_id(integration_id)
        .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Outbound)))
        .one(&state.worker_db)
        .await
        .context("loading integration")?
        .ok_or_else(|| anyhow!("outbound integration {} not found", integration_id))?;

    let forge = ForgeType::try_from(integration.forge_type)
        .map_err(|_| anyhow!("integration has unknown forge_type"))?;

    let token = match integration.access_token.as_deref() {
        Some(enc) => Some(
            decrypt_webhook_secret(&state.config.secrets.crypt_secret_file, enc)
                .map_err(|e| anyhow!("decrypt integration token: {}", e))?,
        ),
        None => None,
    };

    match forge {
        ForgeType::Gitea | ForgeType::Forgejo => {
            let base_url = integration
                .endpoint_url
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("Gitea/Forgejo integration missing endpoint_url"))?;
            let token = token.ok_or_else(|| anyhow!("Gitea/Forgejo integration missing token"))?;
            let r = GiteaReporter::new(state.http.clone(), base_url, token.expose().to_string())?;
            Ok(Arc::new(r))
        }
        ForgeType::GitLab => {
            let base_url = integration
                .endpoint_url
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("GitLab integration missing endpoint_url"))?;
            let token = token.ok_or_else(|| anyhow!("GitLab integration missing token"))?;
            let r = GitlabReporter::new(state.http.clone(), base_url, token.expose().to_string())?;
            Ok(Arc::new(r))
        }
        ForgeType::GitHub => build_github_reporter(state, &integration, token).await,
    }
}

async fn build_github_reporter(
    state: &Arc<ServerState>,
    integration: &entity::integration::Model,
    token: Option<crate::types::SecretString>,
) -> Result<Arc<dyn CiReporter>> {
    if let Some(github_app) = state.config.github_app.clone() {
        let project_org = EOrganization::find_by_id(integration.organization)
            .one(&state.worker_db)
            .await
            .context("loading organization for github app")?
            .ok_or_else(|| anyhow!("integration organization not found"))?;
        if let Some(installation_id) = project_org.github_installation_id {
            let pem = std::fs::read_to_string(&github_app.private_key_file)
                .context("reading github app private key")?;
            let r = GithubAppReporter::new(
                state.http.clone(),
                "",
                github_app.app_id,
                pem,
                installation_id,
            )?;
            return Ok(Arc::new(r));
        }
    }
    let token = token.ok_or_else(|| anyhow!("GitHub integration missing token"))?;
    let r = GithubReporter::new(state.http.clone(), "", token.expose().to_string())?;
    Ok(Arc::new(r))
}

/// Build a payload skeleton suitable for forge-status report executors.
/// Callers fill in `status` and any extra metadata before passing it to
/// `dispatch_build_event`. Captured as a helper so emitters and tests stay in
/// sync about which fields the executor reads.
pub fn forge_status_payload(
    owner: &str,
    repo: &str,
    sha: &str,
    context: &str,
    description: Option<&str>,
    details_url: Option<&str>,
    check_run_id: Option<i64>,
) -> JsonValue {
    let mut v = serde_json::json!({
        "owner": owner,
        "repo": repo,
        "sha": sha,
        "context": context,
    });
    if let Some(d) = description {
        v["description"] = JsonValue::String(d.into());
    }
    if let Some(u) = details_url {
        v["details_url"] = JsonValue::String(u.into());
    }
    if let Some(id) = check_run_id {
        v["check_run_id"] = JsonValue::from(id);
    }
    v
}

fn render_subject(template: Option<&str>, event: &str, payload: &JsonValue) -> String {
    let raw = template.unwrap_or("[Gradient] {event}: {project}");
    let get = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    raw.replace("{event}", event)
        .replace("{project}", &get("project"))
        .replace("{org}", &get("org"))
        .replace("{id}", &get("id"))
        .replace("{status}", &get("status"))
}

fn render_default_body(event: &str, payload: &JsonValue) -> String {
    let get = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("");
    format!(
        "Event: {}\nProject: {}/{}\nEntity: {}\nStatus: {}\nTime: {}\nLink: {}\n",
        event,
        get("org"),
        get("project"),
        get("id"),
        get("status"),
        get("time"),
        get("link"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn action_with(action_type: ActionType, events: Vec<&str>) -> MProjectAction {
        MProjectAction {
            id: crate::types::ProjectActionId::now_v7(),
            project: ProjectId::new(Uuid::nil()),
            name: "t".into(),
            action_type: action_type.to_i16(),
            config: json!({}),
            events: json!(events),
            active: true,
            last_fired_at: None,
            created_by: crate::types::UserId::new(Uuid::nil()),
            created_at: crate::types::now(),
            updated_at: crate::types::now(),
        }
    }

    #[test]
    fn forge_status_mapping() {
        assert!(matches!(
            forge_status_for_event("build.started"),
            Some(CiStatus::Running)
        ));
        assert!(matches!(
            forge_status_for_event("build.completed"),
            Some(CiStatus::Success)
        ));
        assert!(matches!(
            forge_status_for_event("build.failed"),
            Some(CiStatus::Failure)
        ));
        assert!(forge_status_for_event("evaluation.failed").is_none());
    }

    #[test]
    fn matches_event_send_mail_filters_by_stored_events() {
        let a = action_with(ActionType::SendMail, vec!["build.completed"]);
        assert!(matches_event(&a, "build.completed"));
        assert!(!matches_event(&a, "build.failed"));
    }

    #[test]
    fn matches_event_forge_status_ignores_stored_events() {
        let a = action_with(ActionType::ForgeStatusReport, vec!["build.queued"]);
        assert!(matches_event(&a, "build.started"));
        assert!(matches_event(&a, "build.completed"));
        assert!(matches_event(&a, "build.failed"));
        assert!(!matches_event(&a, "build.queued"));
        assert!(!matches_event(&a, "evaluation.completed"));
    }

    #[test]
    fn render_subject_with_default_template() {
        let payload = json!({ "project": "demo", "id": "abc" });
        let s = render_subject(None, "build.failed", &payload);
        assert!(s.contains("build.failed"));
        assert!(s.contains("demo"));
    }

    #[test]
    fn render_subject_with_custom_template() {
        let payload = json!({ "project": "demo", "status": "fail" });
        let s = render_subject(Some("X {project} {status}"), "build.failed", &payload);
        assert_eq!(s, "X demo fail");
    }

    #[test]
    fn render_default_body_includes_fields() {
        let payload = json!({
            "org": "o", "project": "p", "id": "i",
            "status": "s", "time": "t", "link": "l",
        });
        let b = render_default_body("build.completed", &payload);
        assert!(b.contains("build.completed"));
        assert!(b.contains("o/p"));
        assert!(b.contains("Link: l"));
    }

    #[test]
    fn truncate_respects_max() {
        let s = "a".repeat(100);
        assert_eq!(truncate(s.clone(), 50).len(), 50);
        assert_eq!(truncate("short".into(), 50), "short");
    }

    #[test]
    fn forge_status_payload_includes_required_fields() {
        let p = forge_status_payload("acme", "widgets", "deadbeef", "ctx", None, None, None);
        assert_eq!(p["owner"], "acme");
        assert_eq!(p["repo"], "widgets");
        assert_eq!(p["sha"], "deadbeef");
        assert_eq!(p["context"], "ctx");
        assert!(p.get("description").is_none());
    }

    #[test]
    fn forge_status_payload_includes_optional_fields() {
        let p = forge_status_payload(
            "o", "r", "s", "c", Some("desc"), Some("https://x"), Some(42),
        );
        assert_eq!(p["description"], "desc");
        assert_eq!(p["details_url"], "https://x");
        assert_eq!(p["check_run_id"], 42);
    }
}
