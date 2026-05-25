/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Project Actions dispatch and execution.

use crate::ci::integration_lookup::{ForgeType, IntegrationKind};
use crate::ci::reporter::{
    APPROVAL_ACTION_ID, CiReport, CiReporter, CiStatus, GiteaReporter, GithubAppReporter,
    GithubReporter, GitlabReporter, RequestedAction,
};
use crate::ci::{parse_owner_repo, reporting};
use crate::types::input::{load_secret_bytes, vec_to_hex};
use crate::types::{
    AProjectActionDelivery, ActionConfig, ActionType, BuildId, CEntryPoint, CIntegration,
    CProjectAction, EBuild, ECommit, EDerivation, EEntryPoint, EEvaluation, EIntegration,
    EOrganization, EProject, EProjectAction, EvaluationId, IntegrationId, MProjectAction,
    ProjectActionDeliveryId,
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

/// Load the server's crypt key from `crypt_secret_file` and encrypt `plaintext`.
pub fn encrypt_secret_with_file(crypt_secret_file: &str, plaintext: &str) -> Result<String> {
    let key =
        load_secret_bytes(crypt_secret_file).map_err(|e| anyhow!("loading crypt key: {}", e))?;
    encrypt_action_secret(plaintext, key.expose())
}

/// Load the server's crypt key from `crypt_secret_file` and decrypt `ciphertext`,
/// returning a [`crate::types::SecretString`] so the plaintext is zeroized on drop.
pub fn decrypt_secret_with_file(
    crypt_secret_file: &str,
    ciphertext: &str,
) -> Result<crate::types::SecretString> {
    let key =
        load_secret_bytes(crypt_secret_file).map_err(|e| anyhow!("loading crypt key: {}", e))?;
    decrypt_action_secret(ciphertext, key.expose()).map(crate::types::SecretString::new)
}

pub const FORGE_STATUS_EVENTS: &[&str] = &[
    "build.queued",
    "build.started",
    "build.completed",
    "build.failed",
    "build.substituted",
    "evaluation.queued",
    "evaluation.started",
    "evaluation.completed",
    "evaluation.failed",
    "evaluation.aborted",
    "evaluation.action_required",
    "evaluation.approval_granted",
];
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
        "build.queued" => Some(CiStatus::Pending),
        "build.started" => Some(CiStatus::Running),
        "build.completed" => Some(CiStatus::Success),
        "build.failed" => Some(CiStatus::Failure),
        "build.substituted" => Some(CiStatus::Success),
        "evaluation.queued" => Some(CiStatus::Pending),
        "evaluation.started" => Some(CiStatus::Running),
        "evaluation.completed" => Some(CiStatus::Success),
        "evaluation.failed" => Some(CiStatus::Failure),
        "evaluation.aborted" => Some(CiStatus::Error),
        "evaluation.action_required" => Some(CiStatus::ActionRequired),
        // Approval click clears the Awaiting-Approval gate. We post Success
        // on the same check so the maintainer sees it turn green and the
        // PR's required-checks count drops the gate.
        "evaluation.approval_granted" => Some(CiStatus::Success),
        _ => None,
    }
}

fn requested_actions_for(status: CiStatus) -> Vec<RequestedAction> {
    match status {
        CiStatus::ActionRequired => vec![RequestedAction {
            identifier: APPROVAL_ACTION_ID.to_string(),
            label: "Approve and run".to_string(),
            description: "Run CI for external contributor PR.".to_string(),
        }],
        _ => Vec::new(),
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

/// Dispatch the first forge-status event for a freshly-created evaluation.
///
/// Replaces the legacy `spawn_pending_ci_for_eval` reporter that was removed
/// alongside the per-project outbound integration: an eval row that has just
/// been INSERTed (Queued, or Waiting+Approval/NoCache/Workers due to the
/// trigger-time gates) never transitions through `update_evaluation_status`,
/// so `dispatch_evaluation_event_for_status` would not fire for it. Without
/// this helper, the commit shows no Gradient check at all until an eval
/// worker actually starts processing it.
///
/// Maps the eval's creation-time state to the matching event:
/// - `Queued` → `evaluation.queued` (Pending).
/// - `Waiting + Approval` → `evaluation.action_required` (ActionRequired,
///   carries the maintainer-approval description).
/// - `Waiting + NoCache` → `evaluation.queued` (Pending, "no cache" description).
/// - `Waiting + Workers` → `evaluation.queued` (Pending, "no eval-capable
///   worker" description). Issue #268.
pub async fn dispatch_evaluation_created(
    state: &Arc<ServerState>,
    eval: &crate::types::MEvaluation,
) {
    use crate::types::waiting_reason::WaitingReason;
    use entity::evaluation::EvaluationStatus;

    let Some(project_id) = eval.project else {
        return;
    };

    let reason = eval
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);

    let (event, description) = match (eval.status, reason) {
        (EvaluationStatus::Queued, _) => ("evaluation.queued", None),
        (EvaluationStatus::Waiting, Some(WaitingReason::Approval { .. })) => (
            "evaluation.action_required",
            Some("Awaiting maintainer approval for external contributor PR."),
        ),
        (EvaluationStatus::Waiting, Some(WaitingReason::NoCache)) => (
            "evaluation.queued",
            Some("Waiting for a writable cache subscription before this evaluation can run."),
        ),
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::Workers {
                connected_workers: 0,
                ..
            }),
        ) => (
            "evaluation.queued",
            Some("Waiting for an eval-capable worker to be registered on the organisation."),
        ),
        _ => return,
    };

    let mut payload = serde_json::json!({
        "evaluation_id": eval.id,
        "project_id": eval.project,
        "repository": eval.repository,
        "status": event,
    });
    if let Some(text) = description {
        payload["description"] = JsonValue::String(text.to_string());
    }

    dispatch_evaluation_event(state, project_id, event, payload).await;
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

pub(crate) struct ExecutorOk {
    pub(crate) status_code: Option<i32>,
    pub(crate) response_body: Option<String>,
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
    let success = match &result {
        Ok(ok) => ok
            .status_code
            .map(|c| (200..300).contains(&c))
            .unwrap_or(true),
        Err(_) => false,
    };
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
    let r = state
        .email
        .send_action_mail(recipients, &subject, &body)
        .await?;
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
    crate::ci::http_validation::validate_webhook_url(url)
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
    let body = resp.text().await.unwrap_or_default();
    Ok(ExecutorOk {
        status_code: Some(status),
        response_body: Some(truncate(body, MAX_BODY_BYTES)),
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

    let report = build_ci_report_from_payload(state, event, payload, ci_status).await?;
    let context_key = report.context.clone();
    let reporter = build_reporter_for_integration(state, integration_id).await?;
    let new_id = reporter
        .report(&report)
        .await
        .context("forge status report failed")?;
    if let (Some(new_id), Some(eid)) = (
        new_id,
        payload.get("evaluation_id").and_then(|v| v.as_str()),
    ) && let Ok(evaluation_id) = eid.parse::<EvaluationId>()
    {
        persist_evaluation_check_id(state, evaluation_id, &context_key, new_id).await;
    }
    let body = new_id.map(|id| format!("{{\"check_run_id\":{}}}", id));
    Ok(ExecutorOk {
        status_code: Some(200),
        response_body: body,
    })
}

/// Upsert `check_run_id` into `evaluation.check_run_ids` under the given
/// context name. Keeps every (Awaiting Approval / Evaluation / Build {ep})
/// check id distinct so subsequent PATCHes don't stomp the wrong check run.
async fn persist_evaluation_check_id(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    context: &str,
    check_run_id: i64,
) {
    use sea_orm::IntoActiveModel;
    let Ok(Some(eval)) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await
    else {
        return;
    };
    let mut map = eval
        .check_run_ids
        .as_ref()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    if map.get(context).and_then(|v| v.as_i64()) == Some(check_run_id) {
        return;
    }
    map.insert(context.to_string(), JsonValue::from(check_run_id));
    let mut active = eval.into_active_model();
    active.check_run_ids = Set(Some(JsonValue::Object(map)));
    if let Err(e) = active.update(&state.worker_db).await {
        warn!(error = %e, %evaluation_id, "persisting evaluation check_run_ids");
    }
}

/// Read a check_run_id previously stored under `context` in
/// `evaluation.check_run_ids`.
fn check_run_id_for_context(eval: &crate::types::MEvaluation, context: &str) -> Option<i64> {
    eval.check_run_ids
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(context))
        .and_then(|v| v.as_i64())
}

async fn build_ci_report_from_payload(
    state: &Arc<ServerState>,
    event: &str,
    payload: &JsonValue,
    status: CiStatus,
) -> Result<CiReport> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(String::from);

    let requested_actions = requested_actions_for(status.clone());

    if let (Some(owner), Some(repo), Some(sha), Some(context)) =
        (s("owner"), s("repo"), s("sha"), s("context"))
    {
        return Ok(CiReport {
            owner,
            repo,
            sha,
            context,
            status,
            description: s("description"),
            details_url: s("details_url"),
            existing_check_id: payload.get("check_run_id").and_then(|v| v.as_i64()),
            requested_actions,
        });
    }

    let (evaluation, build) = if let Some(eid) = s("evaluation_id") {
        let evaluation_id: EvaluationId =
            eid.parse().map_err(|_| anyhow!("invalid evaluation_id"))?;
        let evaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&state.worker_db)
            .await
            .context("loading evaluation")?
            .ok_or_else(|| anyhow!("evaluation {} not found", evaluation_id))?;
        (evaluation, None)
    } else {
        let build_id: BuildId = s("build_id")
            .ok_or_else(|| anyhow!("payload missing 'build_id', 'evaluation_id', and the full owner/repo/sha/context set"))?
            .parse()
            .map_err(|_| anyhow!("invalid build_id"))?;

        let build = EBuild::find_by_id(build_id)
            .one(&state.worker_db)
            .await
            .context("loading build")?
            .ok_or_else(|| anyhow!("build {} not found", build_id))?;

        let evaluation = EEvaluation::find_by_id(build.evaluation)
            .one(&state.worker_db)
            .await
            .context("loading evaluation")?
            .ok_or_else(|| anyhow!("evaluation {} not found", build.evaluation))?;
        (evaluation, Some(build))
    };

    let project_id = evaluation
        .project
        .ok_or_else(|| anyhow!("evaluation has no project (direct build)"))?;

    let project = EProject::find_by_id(project_id)
        .one(&state.worker_db)
        .await
        .context("loading project")?
        .ok_or_else(|| anyhow!("project {} not found", project_id))?;

    let commit = ECommit::find_by_id(evaluation.commit)
        .one(&state.worker_db)
        .await
        .context("loading commit")?
        .ok_or_else(|| anyhow!("commit {} not found", evaluation.commit))?;

    // Always post check runs / status updates against the project's base
    // repository, not `evaluation.repository`. For fork PRs the evaluation
    // URL points at the fork (so the worker can fetch the commit), but the
    // GitHub App installation lives on the base repo - calling the fork's
    // /check-runs endpoint returns 403.
    let (owner, repo) = parse_owner_repo(&project.repository)
        .ok_or_else(|| anyhow!("could not parse owner/repo from {}", project.repository))?;

    let entry_points = match &build {
        Some(b) => EEntryPoint::find()
            .filter(CEntryPoint::Build.eq(b.id))
            .all(&state.worker_db)
            .await
            .context("loading entry points")?,
        None => Vec::new(),
    };

    // Fallback discriminator for builds that have no entry-point row (no
    // `gradient.entry_points` config, or a derivation not matched by the
    // wildcard). Without this every build event would collapse onto the
    // Evaluation context and the per-build check would never appear.
    let build_label = match &build {
        Some(b) => {
            if let Some(ep) = entry_points.first() {
                Some(ep.eval.clone())
            } else {
                let drv = EDerivation::find_by_id(b.derivation)
                    .one(&state.worker_db)
                    .await
                    .context("loading derivation for build label")?;
                drv.map(|d| format!("{}.{}", d.name, d.architecture))
            }
        }
        None => None,
    };

    let org_name = EOrganization::find_by_id(project.organization)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()
        .map(|o| o.name);

    // Pick the check-run name based on which phase fired the event so the
    // Approval, Evaluation, and per-Build checks each show as their own line
    // on the PR. Falls back to a generic project-scoped name for events
    // outside the three known families.
    let context = match reporting::check_context_kind_for_event(event) {
        Some(reporting::CheckContextKind::Approval) => {
            reporting::approval_check_context(&project.name)
        }
        Some(reporting::CheckContextKind::Build) => build_label
            .as_deref()
            .map(|label| reporting::build_check_context(&project.name, label))
            .unwrap_or_else(|| reporting::evaluation_check_context(&project.name)),
        Some(reporting::CheckContextKind::Evaluation) | None => {
            reporting::evaluation_check_context(&project.name)
        }
    };

    let details_url = org_name.as_ref().map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.config.server.frontend_url, org, evaluation.id
        )
    });

    Ok(CiReport {
        owner,
        repo,
        sha: vec_to_hex(&commit.hash),
        context: context.clone(),
        status,
        description: s("description"),
        details_url,
        existing_check_id: check_run_id_for_context(&evaluation, &context)
            .or_else(|| payload.get("check_run_id").and_then(|v| v.as_i64())),
        requested_actions,
    })
}

/// Find the project's first active `ForgeStatusReport` action and build a
/// `CiReporter` from its integration. Used by the PR-approval trust probe to
/// reuse the same forge credentials Actions already use for status reporting.
pub async fn reporter_for_project(
    state: &Arc<ServerState>,
    project_id: ProjectId,
) -> Result<Option<Arc<dyn CiReporter>>> {
    let action = EProjectAction::find()
        .filter(CProjectAction::Project.eq(project_id))
        .filter(CProjectAction::Active.eq(true))
        .filter(CProjectAction::ActionType.eq(ActionType::ForgeStatusReport.to_i16()))
        .one(&state.worker_db)
        .await
        .context("loading forge_status_report action")?;
    let Some(action) = action else {
        return Ok(None);
    };
    let cfg: ActionConfig =
        serde_json::from_value(action.config.clone()).context("decoding action config")?;
    let ActionConfig::ForgeStatusReport { integration_id } = cfg else {
        return Ok(None);
    };
    Ok(Some(
        build_reporter_for_integration(state, integration_id).await?,
    ))
}

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
            decrypt_secret_with_file(&state.config.secrets.crypt_secret_file, enc)
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

/// Builds the payload skeleton expected by `execute_forge_status_report`.
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
        assert!(matches!(
            forge_status_for_event("evaluation.queued"),
            Some(CiStatus::Pending)
        ));
        assert!(matches!(
            forge_status_for_event("evaluation.completed"),
            Some(CiStatus::Success)
        ));
        assert!(matches!(
            forge_status_for_event("evaluation.failed"),
            Some(CiStatus::Failure)
        ));
        assert!(matches!(
            forge_status_for_event("evaluation.action_required"),
            Some(CiStatus::ActionRequired)
        ));
        assert!(forge_status_for_event("evaluation.waiting").is_none());
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
        assert!(matches_event(&a, "evaluation.completed"));
        assert!(matches_event(&a, "evaluation.action_required"));
        assert!(!matches_event(&a, "build.queued"));
        assert!(!matches_event(&a, "evaluation.waiting"));
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
            "o",
            "r",
            "s",
            "c",
            Some("desc"),
            Some("https://x"),
            Some(42),
        );
        assert_eq!(p["description"], "desc");
        assert_eq!(p["details_url"], "https://x");
        assert_eq!(p["check_run_id"], 42);
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn make_state() -> Arc<ServerState> {
        use crate::storage::{EmailSender, LogStorage, NarStore};
        use crate::types::{RuntimeConfig, SecretString, WebDb, WorkerDb};
        use futures::future::BoxFuture;
        use sea_orm::{DatabaseBackend, MockDatabase};

        #[derive(Debug)]
        struct NoopLog;
        impl LogStorage for NoopLog {
            fn append<'a>(
                &'a self,
                _: entity::ids::BuildId,
                _: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn read<'a>(
                &'a self,
                _: entity::ids::BuildId,
            ) -> BoxFuture<'a, anyhow::Result<String>> {
                Box::pin(async { Ok(String::new()) })
            }
            fn delete<'a>(&'a self, _: entity::ids::BuildId) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn list_logs<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<Vec<entity::ids::BuildId>>> {
                Box::pin(async { Ok(Vec::new()) })
            }
        }

        #[derive(Debug)]
        struct NoopEmail;
        #[async_trait::async_trait]
        impl EmailSender for NoopEmail {
            fn is_enabled(&self) -> bool {
                false
            }
            async fn send_verification_email(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn send_password_reset_email(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn send_action_mail(
                &self,
                _: &[String],
                _: &str,
                _: &str,
            ) -> anyhow::Result<crate::storage::email::MailDeliveryResult> {
                Ok(crate::storage::email::MailDeliveryResult {
                    status_code: 0,
                    server_response: String::new(),
                })
            }
        }

        let cli = crate::types::Cli {
            logging: crate::types::LoggingArgs::default(),
            server: crate::types::ServerArgs::default(),
            database: crate::types::DatabaseArgs::default(),
            eval: crate::types::EvalArgs::default(),
            storage: crate::types::StorageArgs {
                base_path: "/tmp/gradient-test".into(),
                ..Default::default()
            },
            secrets: crate::types::SecretsArgs {
                crypt_secret_file: "test-secret".into(),
                jwt_secret_file: "test-jwt".into(),
            },
            limits: crate::types::LimitsArgs::default(),
            registration: crate::types::RegistrationArgs::default(),
            proto: crate::types::ProtoArgs::default(),
            oidc: crate::types::OidcArgs::default(),
            email: crate::types::EmailArgs::default(),
            s3: crate::types::S3Args::default(),
            github_app: crate::types::GitHubAppArgs::default(),
            metrics: crate::types::MetricsArgs::default(),
            network: crate::types::NetworkArgs::default(),
        };
        let config = std::sync::Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
        let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
        Arc::new(crate::types::ServerState {
            web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            worker_db: WorkerDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            config,
            log_storage: std::sync::Arc::new(NoopLog),
            email: std::sync::Arc::new(NoopEmail) as std::sync::Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            pending_credentials: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            http: crate::http::build_client().expect("http client"),
            shutdown: crate::shutdown::Shutdown::new(),
            jwt_secret: SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
        })
    }

    #[test]
    fn build_ci_report_fast_path_uses_payload_fields() {
        run(async {
            let state = make_state();
            let payload = json!({
                "owner": "acme",
                "repo": "widgets",
                "sha": "deadbeef",
                "context": "gradient/my-pkg",
                "description": "Building…",
                "details_url": "https://example.com/log/1",
                "check_run_id": 99,
            });
            let report =
                build_ci_report_from_payload(&state, "build.started", &payload, CiStatus::Running)
                    .await
                    .expect("fast path should succeed");
            assert_eq!(report.owner, "acme");
            assert_eq!(report.repo, "widgets");
            assert_eq!(report.sha, "deadbeef");
            assert_eq!(report.context, "gradient/my-pkg");
            assert_eq!(report.description.as_deref(), Some("Building…"));
            assert_eq!(report.existing_check_id, Some(99));
        });
    }

    #[test]
    fn build_ci_report_errors_when_payload_empty() {
        run(async {
            let state = make_state();
            let err = build_ci_report_from_payload(
                &state,
                "build.started",
                &json!({}),
                CiStatus::Running,
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains("build_id"), "error: {err}");
        });
    }

    #[test]
    fn build_ci_report_errors_on_invalid_build_id() {
        run(async {
            let state = make_state();
            let payload = json!({ "build_id": "not-a-uuid" });
            let err =
                build_ci_report_from_payload(&state, "build.started", &payload, CiStatus::Running)
                    .await
                    .unwrap_err();
            assert!(err.to_string().contains("invalid build_id"), "error: {err}");
        });
    }
}
