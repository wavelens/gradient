/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DeletedResponse;
use super::refusal::TaskRefusal;
use crate::api_spec::TaskPath;
use crate::audit::Audit;
use crate::endpoints::webhooks::{ActionDeliveryItem, DeliveryDetail, DeliveryListItem};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::resource::Resource;
use crate::scope::{Member, Requires, TaskScope, can};
use anyhow::Context;
use axum::Json;
use axum::extract::Query;
use axum::extract::State;
use chrono::Utc;
use gradient_ci::IntegrationKind;
use gradient_ci::actions::encrypt_action_secret;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_types::actions::{ActionConfig, ActionType, is_matrix_room_id};
use gradient_types::events::audit::Action;
use gradient_types::events::{EventOwner, names};
use gradient_types::input::load_secret_bytes;
use gradient_types::*;
use gradient_util::http_validation::{WebhookUrlError, validate_webhook_url};
use sea_orm::ActiveValue::Set;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn router() -> OpenApiRouter<Arc<ServerState>> {
    OpenApiRouter::new()
        .routes(routes!(list_actions, create_action))
        .routes(routes!(read_action, update_action, delete_action))
        .routes(routes!(test_action))
        .routes(routes!(regenerate_token))
        .routes(routes!(list_deliveries))
        .routes(routes!(get_delivery))
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ActionResponse {
    pub id: TaskActionId,
    pub name: String,
    pub action_type: String,
    pub config: JsonValue,
    pub events: Vec<String>,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

impl From<MTaskAction> for ActionResponse {
    fn from(m: MTaskAction) -> Self {
        let at = m.action_type;
        let mut config = m.config;
        if let Some(field) = at.secret_field()
            && let Some(obj) = config.as_object_mut()
        {
            obj.remove(field);
        }
        let events = m
            .events
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            id: m.id,
            name: m.name,
            action_type: at.as_str().into(),
            config,
            events,
            active: m.active,
            last_fired_at: m.last_fired_at,
            created_by: m.created_by,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct CreateActionResponse {
    pub action: ActionResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CreateActionRequest {
    pub name: String,
    pub config: ActionConfig,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

fn encrypt_secret(config: &RuntimeConfig, plain: &str) -> WebResult<String> {
    let key = load_secret_bytes(&config.secrets.crypt_file).context("load the encryption key")?;
    encrypt_action_secret(plain, key.expose())
        .context("encrypt the action secret")
        .map_err(Into::into)
}

async fn validate_mail_teams(db: &WebDb, project: ProjectId, cfg: &ActionConfig) -> WebResult<()> {
    let ActionConfig::SendMail { recipients, .. } = cfg else {
        return Ok(());
    };
    for name in recipients.iter().filter_map(|r| r.strip_prefix("team:")) {
        if !gradient_db::teams::mail::team_reaches_project_users(db, project, name).await? {
            return Err(TaskRefusal::TeamWithoutProjectAccess {
                team: name.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

async fn validate_start_target(
    db: &WebDb,
    project: ProjectId,
    cfg: &ActionConfig,
) -> WebResult<()> {
    let ActionConfig::StartEvaluation { task } = cfg else {
        return Ok(());
    };
    let target = gradient_db::lookup::by_id::<ETask, _>(db, *task).await?;
    if target.is_none_or(|target| target.project != project) {
        return Err(TaskRefusal::StartTaskInOtherProject.into());
    }
    Ok(())
}

fn validate_destination(cfg: &ActionConfig) -> WebResult<()> {
    let url_error = |e: WebhookUrlError| TaskRefusal::InvalidWebhookUrl(e.to_string());
    match cfg {
        ActionConfig::SendWebRequest { url, .. } => {
            validate_webhook_url(url).map_err(url_error)?;
        }
        ActionConfig::SendMatrixMessage {
            homeserver,
            room_id,
            access_token,
        } => {
            validate_webhook_url(homeserver).map_err(url_error)?;
            if access_token.as_deref().is_some_and(|t| t.trim().is_empty()) {
                return Err(TaskRefusal::EmptyAccessToken.into());
            }

            if !is_matrix_room_id(room_id) {
                return Err(TaskRefusal::InvalidMatrixRoomId.into());
            }
        }
        ActionConfig::SendSlackMessage {
            webhook_url: Some(url),
        } => {
            validate_webhook_url(url).map_err(url_error)?;
        }
        _ => {}
    }

    Ok(())
}

#[utoipa::path(
    get,
    path = "/",
    operation_id = "listTaskActions",
    tag = "actions",
    summary = "List task actions",
    params(TaskPath),
    responses((status = OK, description = "List of actions", body = Vec<ActionResponse>), WebError)
)]
pub async fn list_actions(
    TaskScope { task: proj, db, .. }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<ActionResponse>>>> {
    let rows = gradient_db::actions::of_task(&db.web_db, proj.id).await?;

    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    post,
    path = "/",
    operation_id = "createTaskAction",
    tag = "actions",
    summary = "Create task action",
    params(TaskPath),
    request_body = CreateActionRequest,
    responses((status = OK, description = "Action created; token present only for send_web_request", body = CreateActionResponse), WebError)
)]
pub async fn create_action(
    TaskScope {
        project,
        task: proj,
        user,
        ..
    }: TaskScope<Requires<can::ManageActions>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Json(body): Json<CreateActionRequest>,
) -> WebResult<Json<BaseResponse<CreateActionResponse>>> {
    match &body.config {
        ActionConfig::SendMail { .. } if !state.email.is_enabled() => {
            return Err(TaskRefusal::SmtpNotConfigured.into());
        }
        ActionConfig::GitHostStatusReport { .. } if !body.events.is_empty() => {
            return Err(TaskRefusal::StatusReportWithEvents.into());
        }
        _ => {}
    }

    validate_destination(&body.config)?;
    validate_mail_teams(&state.web_db, project.id, &body.config).await?;
    validate_start_target(&state.web_db, project.id, &body.config).await?;
    if matches!(
        body.config,
        ActionConfig::SendMatrixMessage {
            access_token: None,
            ..
        } | ActionConfig::SendSlackMessage { webhook_url: None }
    ) {
        let at = body.config.action_type();
        return Err(TaskRefusal::MissingSecret {
            action_type: at.as_str().to_string(),
            secret_field: at.secret_field().unwrap_or_default().to_string(),
        }
        .into());
    }

    if let ActionConfig::SendMail { recipients, .. } = &body.config
        && recipients.is_empty()
    {
        return Err(TaskRefusal::NoMailRecipients.into());
    }

    let integration_id = match &body.config {
        ActionConfig::GitHostStatusReport { integration_id }
        | ActionConfig::OpenPr { integration_id, .. } => Some(*integration_id),
        _ => None,
    };
    if let Some(integration_id) = integration_id {
        let integration =
            gradient_db::integrations::by_id_in_project(&state.web_db, project.id, integration_id)
                .await?;
        match integration {
            Some(row) if row.kind == IntegrationKind::Outbound => {}
            Some(_) => {
                return Err(TaskRefusal::IntegrationNotOutbound.into());
            }
            None => {
                return Err(TaskRefusal::OutboundIntegrationNotFound.into());
            }
        }
    }

    let existing = gradient_db::actions::by_name(&state.web_db, proj.id, &body.name).await?;
    if existing.is_some() {
        return Err(TaskRefusal::ActionNameTaken.into());
    }

    let plaintext_token = match &body.config {
        ActionConfig::SendWebRequest { token, .. } => token.clone(),
        _ => None,
    };
    let mut stored_config = body.config.clone();
    if let Some(slot) = stored_config.secret_mut()
        && let Some(plain) = slot.take()
    {
        *slot = Some(encrypt_secret(&state.config, &plain)?);
    }

    let now = Utc::now().naive_utc();
    let action = MTaskAction {
        id: TaskActionId::now_v7(),
        task: proj.id,
        name: body.name,
        action_type: stored_config.action_type(),
        config: serde_json::to_value(&stored_config).context("serialize the action config")?,
        events: serde_json::to_value(&body.events).context("serialize the action events")?,
        active: body.active,
        created_by: user.id,
        created_at: now,
        updated_at: now,
        ..Default::default()
    };

    let m = gradient_db::actions::insert(&state.web_db, action)
        .await
        .map_err(|e| WebError::from_unique_violation(e, TaskRefusal::ActionNameTaken))?;

    record_action_event(&audit, Action::TaskActionCreate, project.id, &m).await;

    Ok(ok_json(CreateActionResponse {
        action: m.into(),
        token: plaintext_token,
    }))
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct UpdateActionRequest {
    pub name: Option<String>,
    pub config: Option<ActionConfig>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/{id}",
    operation_id = "getTaskAction",
    tag = "actions",
    summary = "Get task action",
    params(TaskPath, ("id" = TaskActionId, Path)),
    responses((status = OK, description = "Action (token stripped)", body = ActionResponse), WebError)
)]
pub async fn read_action(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<ActionResponse>>> {
    let id: TaskActionId = params.param("id")?;

    let row = gradient_db::actions::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Action)?;

    Ok(ok_json(ActionResponse::from(row)))
}

#[utoipa::path(
    patch,
    path = "/{id}",
    operation_id = "updateTaskAction",
    tag = "actions",
    summary = "Update task action",
    params(TaskPath, ("id" = TaskActionId, Path)),
    request_body = UpdateActionRequest,
    responses((status = OK, description = "Action updated", body = ActionResponse), WebError)
)]
pub async fn update_action(
    TaskScope {
        project,
        task: proj,
        params,
        ..
    }: TaskScope<Requires<can::ManageActions>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Json(body): Json<UpdateActionRequest>,
) -> WebResult<Json<BaseResponse<ActionResponse>>> {
    let id: TaskActionId = params.param("id")?;

    let row = gradient_db::actions::by_id_in_task(&state.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Action)?;

    let existing_type = row.action_type;

    if let Some(ref new_cfg) = body.config {
        if new_cfg.action_type() != existing_type {
            return Err(TaskRefusal::ActionTypeChange.into());
        }

        validate_destination(new_cfg)?;
        validate_mail_teams(&state.web_db, project.id, new_cfg).await?;
        validate_start_target(&state.web_db, project.id, new_cfg).await?;
        match new_cfg {
            ActionConfig::SendMail { recipients, .. } if recipients.is_empty() => {
                return Err(TaskRefusal::NoMailRecipients.into());
            }
            ActionConfig::GitHostStatusReport { integration_id }
            | ActionConfig::OpenPr { integration_id, .. } => {
                let integration = gradient_db::integrations::by_id_in_project(
                    &state.web_db,
                    project.id,
                    *integration_id,
                )
                .await?;
                match integration {
                    Some(r) if r.kind == IntegrationKind::Outbound => {}
                    Some(_) => {
                        return Err(TaskRefusal::IntegrationNotOutbound.into());
                    }
                    None => {
                        return Err(TaskRefusal::OutboundIntegrationNotFound.into());
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(ref evs) = body.events
        && matches!(
            existing_type,
            ActionType::GitHostStatusReport | ActionType::OpenPr
        )
        && !evs.is_empty()
    {
        return Err(TaskRefusal::CustomEventsNotAllowed.into());
    }

    let read_at = row.updated_at;
    let mut active: ATaskAction = row.into();

    if let Some(new_cfg) = body.config {
        let stored: ActionConfig = serde_json::from_value(active.config.as_ref().clone())
            .context("parse the stored action config")?;
        let mut stored_cfg = new_cfg;
        if let Some(slot) = stored_cfg.secret_mut()
            && let Some(plain) = slot.take()
        {
            *slot = Some(encrypt_secret(&state.config, &plain)?);
        } else {
            stored_cfg.keep_secret_from(stored);
        }

        active.config =
            Set(serde_json::to_value(&stored_cfg).context("serialize the action config")?);
    }

    if let Some(name) = body.name {
        active.name = Set(name);
    }
    if let Some(evs) = body.events {
        active.events = Set(serde_json::to_value(&evs).context("serialize the action events")?);
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = gradient_db::actions::update(&state.web_db, active, read_at)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::Action))?
        .or_not_found(Resource::Action)?;

    record_action_event(&audit, Action::TaskActionUpdate, project.id, &updated).await;

    Ok(ok_json(ActionResponse::from(updated)))
}

async fn record_action_event(audit: &Audit, action: Action, project: ProjectId, row: &MTaskAction) {
    let owner = EventOwner::task(project, row.task);
    let metadata = serde_json::json!({
        "action_id": row.id.to_string(),
        "name": row.name,
        "action_type": row.action_type,
    });
    audit.write(action, owner, metadata).await;
}

#[utoipa::path(
    delete,
    path = "/{id}",
    operation_id = "deleteTaskAction",
    tag = "actions",
    summary = "Delete task action",
    params(TaskPath, ("id" = TaskActionId, Path)),
    responses((status = OK, description = "Action deleted", body = DeletedResponse), WebError)
)]
pub async fn delete_action(
    TaskScope {
        project,
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Requires<can::ManageActions>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let id: TaskActionId = params.param("id")?;

    let row = gradient_db::actions::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Action)?;

    gradient_db::actions::delete(&db.web_db, row.clone()).await?;

    record_action_event(&audit, Action::TaskActionDelete, project.id, &row).await;

    Ok(ok_json(DeletedResponse::deleted()))
}

#[utoipa::path(
    post,
    path = "/{id}/test",
    operation_id = "testTaskAction",
    tag = "actions",
    summary = "Fire action now (synthetic test)",
    description = "Fires the action immediately with a synthetic test event, bypassing event matching. Git host integration actions (`git_host_status_report`, `open_pr`) instead perform a non-mutating connectivity check against the task's repository, since a synthetic status/PR posted against a placeholder commit is always rejected by the Git host.",
    params(TaskPath, ("id" = TaskActionId, Path)),
    responses((status = OK, description = "Action fired", body = serde_json::Value), WebError)
)]
pub async fn test_action(
    TaskScope {
        task: proj, params, ..
    }: TaskScope<Requires<can::ManageActions>>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let ci = state.ci();
    let id: TaskActionId = params.param("id")?;
    let project: String = params.param("project")?;
    let task: String = params.param("task")?;

    let action = gradient_db::actions::by_id_in_task(&state.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Action)?;

    let action_type = action.action_type;

    // Git-host-integration actions cannot be test-fired against a synthetic commit. The Git host is
    // rejecting the placeholder owner, repo and sha.
    if matches!(
        action_type,
        ActionType::GitHostStatusReport | ActionType::OpenPr
    ) {
        gradient_ci::actions::verify_git_host_action(&ci, &action, &proj.repository)
            .await
            .context("run the test fire")?;

        return Ok(ok_json(serde_json::Value::Null));
    }

    let event = action
        .events
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or(names::EVALUATION_COMPLETED)
        .to_string();

    let now = chrono::Utc::now();
    let content = serde_json::json!({
        "synthetic": true,
        "event": event,
        "project": project,
        "task": task,
        "id": "00000000-0000-0000-0000-000000000000",
        "status": "ok",
        "time": now.to_rfc3339(),
        "link": test_fire_link(&ci, proj.id, &project, &task).await?,
        "owner": "gradient-test",
        "repo": task,
        "sha": "0000000000000000000000000000000000000000",
        "context": "gradient/test-fire",
    });

    let envelope = serde_json::json!({
        "event": event,
        "at": now.to_rfc3339(),
        "content": content,
    });
    gradient_ci::actions::execute_action(&ci, action, &event, envelope)
        .await
        .context("run the test fire")?;

    Ok(ok_json(serde_json::Value::Null))
}

async fn test_fire_link(
    ci: &gradient_ci::CiContext,
    task_id: TaskId,
    project: &str,
    task: &str,
) -> WebResult<String> {
    let frontend_url = &ci.db.config.server.frontend_url;
    let latest = gradient_db::evaluations::latest_of_task(&ci.db.web_db, task_id, 1).await?;
    Ok(match latest.first() {
        Some(evaluation) => format!("{frontend_url}/project/{project}/log/{}", evaluation.id),
        None => format!("{frontend_url}/project/{project}/task/{task}"),
    })
}

#[utoipa::path(
    post,
    path = "/{id}/regenerate-token",
    operation_id = "regenerateActionToken",
    tag = "actions",
    summary = "Regenerate send_web_request token",
    description = "Invalidates the current token and returns a new one in plaintext. Only valid for send_web_request actions.",
    params(TaskPath, ("id" = TaskActionId, Path)),
    responses((status = OK, description = "New token (one-time reveal)", body = serde_json::Value), WebError)
)]
pub async fn regenerate_token(
    TaskScope {
        task: proj, params, ..
    }: TaskScope<Requires<can::ManageActions>>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngExt as _;
    let id: TaskActionId = params.param("id")?;

    let existing = gradient_db::actions::by_id_in_task(&state.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Action)?;

    if existing.action_type != ActionType::SendWebRequest {
        return Err(TaskRefusal::TokenRegenerationNotSupported.into());
    }

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    let plaintext_token = format!("gat_{}", URL_SAFE_NO_PAD.encode(raw));

    let encrypted = encrypt_secret(&state.config, &plaintext_token)?;

    let mut cfg: ActionConfig = serde_json::from_value(existing.config.clone())
        .context("parse the stored action config")?;
    if let ActionConfig::SendWebRequest { token: t, .. } = &mut cfg {
        *t = Some(encrypted);
    }

    let read_at = existing.updated_at;
    let mut am: ATaskAction = existing.into();
    am.config = Set(serde_json::to_value(&cfg).context("serialize the action config")?);
    am.updated_at = Set(Utc::now().naive_utc());
    gradient_db::actions::update(&state.web_db, am, read_at)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::Action))?
        .or_not_found(Resource::Action)?;

    Ok(ok_json(serde_json::json!({ "token": plaintext_token })))
}

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ActionDeliveryListQuery {
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/{id}/deliveries",
    operation_id = "listActionDeliveries",
    tag = "actions",
    summary = "List action deliveries",
    params(TaskPath, ("id" = TaskActionId, Path), ActionDeliveryListQuery),
    responses((status = OK, description = "Paginated delivery list", body = Vec<ActionDeliveryItem>), WebError)
)]
pub async fn list_deliveries(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Member>,
    Query(q): Query<ActionDeliveryListQuery>,
) -> WebResult<Json<BaseResponse<Vec<DeliveryListItem<TaskActionDeliveryId>>>>> {
    let id: TaskActionId = params.param("id")?;

    gradient_db::actions::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Action)?;

    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);

    let rows = gradient_db::actions::deliveries_of(&db.web_db, id, limit, offset).await?;

    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/{id}/deliveries/{delivery_id}",
    operation_id = "getActionDelivery",
    tag = "actions",
    summary = "Get action delivery detail",
    params(TaskPath, ("id" = TaskActionId, Path), ("delivery_id" = TaskActionDeliveryId, Path)),
    responses((status = OK, description = "Delivery with request/response bodies", body = DeliveryDetail<TaskActionDeliveryId>), WebError)
)]
pub async fn get_delivery(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<DeliveryDetail<TaskActionDeliveryId>>>> {
    let action_id: TaskActionId = params.param("id")?;
    let delivery_id: TaskActionDeliveryId = params.param("delivery_id")?;

    gradient_db::actions::by_id_in_task(&db.web_db, proj.id, action_id)
        .await?
        .or_not_found(Resource::Action)?;

    let r = gradient_db::actions::delivery_of(&db.web_db, action_id, delivery_id)
        .await?
        .or_not_found(Resource::Delivery)?;

    Ok(ok_json(r.into()))
}
