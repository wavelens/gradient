/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, TaskAccess, load_task};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::Query;
use axum::extract::{Path, State};
use axum::{Extension, Json, Router};
use chrono::Utc;
use gradient_ci::IntegrationKind;
use gradient_ci::actions::encrypt_action_secret;
use gradient_core::ServerState;
use gradient_types::actions::{ActionConfig, ActionType, is_matrix_room_id};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::load_secret_bytes;
use gradient_types::*;
use gradient_util::http_validation::{WebhookUrlError, validate_webhook_url};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, Order, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::sync::Arc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", axum::routing::get(list_actions).post(create_action))
        .route(
            "/{id}",
            axum::routing::get(read_action)
                .patch(update_action)
                .delete(delete_action),
        )
        .route("/{id}/test", axum::routing::post(test_action))
        .route(
            "/{id}/regenerate-token",
            axum::routing::post(regenerate_token),
        )
        .route("/{id}/deliveries", axum::routing::get(list_deliveries))
        .route(
            "/{id}/deliveries/{delivery_id}",
            axum::routing::get(get_delivery),
        )
}

#[derive(Serialize, Debug)]
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

#[derive(Serialize, Debug)]
pub struct CreateActionResponse {
    pub action: ActionResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Deserialize, Debug)]
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

fn encrypt_secret(state: &ServerState, plain: &str) -> WebResult<String> {
    let key = load_secret_bytes(&state.config.secrets.crypt_file)
        .map_err(|e| WebError::internal(e.to_string()))?;
    encrypt_action_secret(plain, key.expose()).map_err(|e| WebError::internal(e.to_string()))
}

fn validate_destination(cfg: &ActionConfig) -> WebResult<()> {
    let url_error = |e: WebhookUrlError| WebError::unprocessable_entity(e.to_string());
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
                return Err(WebError::unprocessable_entity(
                    "access_token must not be empty",
                ));
            }

            if !is_matrix_room_id(room_id) {
                return Err(WebError::unprocessable_entity(
                    "room_id must be a Matrix room ID like !abc:example.org",
                ));
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

fn to_response(m: MTaskAction) -> ActionResponse {
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
    ActionResponse {
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

pub async fn list_actions(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<Vec<ActionResponse>>>> {
    let (_project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Member,
    )
    .await?;

    let rows = ETaskAction::find()
        .filter(CTaskAction::Task.eq(proj.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(rows.into_iter().map(to_response).collect()))
}

pub async fn create_action(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Json(body): Json<CreateActionRequest>,
) -> WebResult<Json<BaseResponse<CreateActionResponse>>> {
    let (project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    match &body.config {
        ActionConfig::SendMail { .. } if !state.email.is_enabled() => {
            return Err(WebError::unprocessable_entity(
                "SMTP is not configured on this server",
            ));
        }
        ActionConfig::GitHostStatusReport { .. } if !body.events.is_empty() => {
            return Err(WebError::unprocessable_entity(
                "git_host_status_report actions cannot carry custom events",
            ));
        }
        _ => {}
    }

    validate_destination(&body.config)?;
    if matches!(
        body.config,
        ActionConfig::SendMatrixMessage {
            access_token: None,
            ..
        } | ActionConfig::SendSlackMessage { webhook_url: None }
    ) {
        let at = body.config.action_type();
        return Err(WebError::unprocessable_entity(format!(
            "{} requires {}",
            at.as_str(),
            at.secret_field().unwrap_or_default(),
        )));
    }

    if let ActionConfig::SendMail { recipients, .. } = &body.config
        && recipients.is_empty()
    {
        return Err(WebError::unprocessable_entity(
            "send_mail requires at least one recipient",
        ));
    }

    let integration_id = match &body.config {
        ActionConfig::GitHostStatusReport { integration_id }
        | ActionConfig::OpenPr { integration_id, .. } => Some(*integration_id),
        _ => None,
    };
    if let Some(integration_id) = integration_id {
        let integration = EIntegration::find()
            .filter(CIntegration::Id.eq(integration_id))
            .filter(CIntegration::Project.eq(project.id))
            .one(&state.web_db)
            .await?;
        match integration {
            Some(row) if row.kind == IntegrationKind::Outbound => {}
            Some(_) => {
                return Err(WebError::unprocessable_entity(
                    "integration is not an outbound integration",
                ));
            }
            None => {
                return Err(WebError::unprocessable_entity(
                    "outbound integration not found",
                ));
            }
        }
    }

    let existing = ETaskAction::find()
        .filter(CTaskAction::Task.eq(proj.id))
        .filter(CTaskAction::Name.eq(body.name.clone()))
        .one(&state.web_db)
        .await?;
    if existing.is_some() {
        return Err(WebError::Conflict(
            crate::error::ErrorCode::ALREADY_EXISTS,
            "action with this name already exists".into(),
        ));
    }

    let plaintext_token = match &body.config {
        ActionConfig::SendWebRequest { token, .. } => token.clone(),
        _ => None,
    };
    let mut stored_config = body.config.clone();
    if let Some(slot) = stored_config.secret_mut()
        && let Some(plain) = slot.take()
    {
        *slot = Some(encrypt_secret(&state, &plain)?);
    }

    let now = Utc::now().naive_utc();
    let am = MTaskAction {
        id: TaskActionId::now_v7(),
        task: proj.id,
        name: body.name,
        action_type: stored_config.action_type(),
        config: serde_json::to_value(&stored_config)
            .map_err(|e| WebError::internal(e.to_string()))?,
        events: serde_json::to_value(&body.events)
            .map_err(|e| WebError::internal(e.to_string()))?,
        active: body.active,
        created_by: user.id,
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
    .into_active_model();

    let m = am
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Action"))?;

    record_action_event(
        &state,
        &user,
        &info,
        Action::TaskActionCreate,
        project.id,
        &m,
    )
    .await;

    Ok(ok_json(CreateActionResponse {
        action: to_response(m),
        token: plaintext_token,
    }))
}

#[derive(Deserialize, Debug)]
pub struct UpdateActionRequest {
    pub name: Option<String>,
    pub config: Option<ActionConfig>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

pub async fn read_action(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, id)): Path<(String, String, TaskActionId)>,
) -> WebResult<Json<BaseResponse<ActionResponse>>> {
    let (_project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Member,
    )
    .await?;

    let row = ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    Ok(ok_json(to_response(row)))
}

pub async fn update_action(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, id)): Path<(String, String, TaskActionId)>,
    Json(body): Json<UpdateActionRequest>,
) -> WebResult<Json<BaseResponse<ActionResponse>>> {
    let (project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let row = ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let existing_type = row.action_type;

    if let Some(ref new_cfg) = body.config {
        if new_cfg.action_type() != existing_type {
            return Err(WebError::unprocessable_entity(
                "action_type cannot be changed",
            ));
        }

        validate_destination(new_cfg)?;
        match new_cfg {
            ActionConfig::SendMail { recipients, .. } if recipients.is_empty() => {
                return Err(WebError::unprocessable_entity(
                    "send_mail requires at least one recipient",
                ));
            }
            ActionConfig::GitHostStatusReport { integration_id }
            | ActionConfig::OpenPr { integration_id, .. } => {
                let integration = EIntegration::find()
                    .filter(CIntegration::Id.eq(*integration_id))
                    .filter(CIntegration::Project.eq(project.id))
                    .one(&state.web_db)
                    .await?;
                match integration {
                    Some(r) if r.kind == IntegrationKind::Outbound => {}
                    Some(_) => {
                        return Err(WebError::unprocessable_entity(
                            "integration is not an outbound integration",
                        ));
                    }
                    None => {
                        return Err(WebError::unprocessable_entity(
                            "outbound integration not found",
                        ));
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
        return Err(WebError::unprocessable_entity(
            "git_host_status_report and open_pr actions cannot carry custom events",
        ));
    }

    let mut active: ATaskAction = row.into();

    if let Some(new_cfg) = body.config {
        let stored: ActionConfig = serde_json::from_value(active.config.as_ref().clone())
            .map_err(|e| WebError::internal(e.to_string()))?;
        let mut stored_cfg = new_cfg;
        if let Some(slot) = stored_cfg.secret_mut()
            && let Some(plain) = slot.take()
        {
            *slot = Some(encrypt_secret(&state, &plain)?);
        } else {
            stored_cfg.keep_secret_from(stored);
        }

        active.config =
            Set(serde_json::to_value(&stored_cfg).map_err(|e| WebError::internal(e.to_string()))?);
    }

    if let Some(name) = body.name {
        active.name = Set(name);
    }
    if let Some(evs) = body.events {
        active.events =
            Set(serde_json::to_value(&evs).map_err(|e| WebError::internal(e.to_string()))?);
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = active
        .update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Action"))?;

    record_action_event(
        &state,
        &user,
        &info,
        Action::TaskActionUpdate,
        project.id,
        &updated,
    )
    .await;

    Ok(ok_json(to_response(updated)))
}

async fn record_action_event(
    state: &ServerState,
    user: &MUser,
    info: &RequestInfo,
    action: Action,
    project: ProjectId,
    row: &MTaskAction,
) {
    let owner = EventOwner {
        project: Some(project),
        task: Some(row.task),
        ..Default::default()
    };
    let metadata = serde_json::json!({
        "action_id": row.id.to_string(),
        "name": row.name,
        "action_type": row.action_type,
    });
    audit_record(state, Some(user.id), action, owner, info, Some(metadata)).await;
}

#[derive(Serialize, Debug)]
pub struct DeletedResponse {
    deleted: bool,
}

pub async fn delete_action(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, id)): Path<(String, String, TaskActionId)>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let (project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let row = ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let active: ATaskAction = row.clone().into();
    active.delete(&state.web_db).await?;

    record_action_event(
        &state,
        &user,
        &info,
        Action::TaskActionDelete,
        project.id,
        &row,
    )
    .await;

    Ok(ok_json(DeletedResponse { deleted: true }))
}

pub async fn test_action(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, id)): Path<(String, String, TaskActionId)>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let (_project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project.clone(),
        task.clone(),
        TaskAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let action = ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let action_type = action.action_type;

    // Git-host-integration actions cannot be test-fired against a synthetic commit. The Git host is
    // rejecting the placeholder owner, repo and sha.
    if matches!(
        action_type,
        ActionType::GitHostStatusReport | ActionType::OpenPr
    ) {
        gradient_ci::actions::verify_git_host_action(&state.ci(), &action, &proj.repository)
            .await
            .map_err(|e| WebError::internal(format!("test fire failed: {}", e)))?;

        return Ok(ok_json(serde_json::Value::Null));
    }

    let event = action
        .events
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("evaluation.completed")
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
        "link": format!("https://gradient.example/tasks/{}/{}", project, task),
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
    gradient_ci::actions::execute_action(&state.ci(), action, &event, envelope)
        .await
        .map_err(|e| WebError::internal(format!("test fire failed: {}", e)))?;

    Ok(ok_json(serde_json::Value::Null))
}

pub async fn regenerate_token(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, id)): Path<(String, String, TaskActionId)>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngExt as _;

    let (_project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let existing = ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    if existing.action_type != ActionType::SendWebRequest {
        return Err(WebError::unprocessable_entity(
            "regenerate-token is only valid for send_web_request actions",
        ));
    }

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    let plaintext_token = format!("gat_{}", URL_SAFE_NO_PAD.encode(raw));

    let encrypted = encrypt_secret(&state, &plaintext_token)?;

    let mut cfg: ActionConfig = serde_json::from_value(existing.config.clone())
        .map_err(|e| WebError::internal(e.to_string()))?;
    if let ActionConfig::SendWebRequest { token: t, .. } = &mut cfg {
        *t = Some(encrypted);
    }

    let mut am: ATaskAction = existing.into();
    am.config = Set(serde_json::to_value(&cfg).map_err(|e| WebError::internal(e.to_string()))?);
    am.updated_at = Set(Utc::now().naive_utc());
    am.update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Action"))?;

    Ok(ok_json(serde_json::json!({ "token": plaintext_token })))
}

#[derive(Deserialize, Debug)]
pub struct DeliveryListQuery {
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Serialize, Debug)]
pub struct DeliveryListItem {
    pub id: TaskActionDeliveryId,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: i32,
    pub delivered_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Debug)]
pub struct DeliveryDetail {
    #[serde(flatten)]
    pub item: DeliveryListItem,
    pub request_body: String,
    pub response_body: Option<String>,
}

fn to_delivery_list_item(r: MTaskActionDelivery) -> DeliveryListItem {
    DeliveryListItem {
        id: r.id,
        event: r.event,
        success: r.success,
        response_status: r.response_status,
        error_message: r.error_message,
        duration_ms: r.duration_ms,
        delivered_at: r.delivered_at,
    }
}

pub async fn list_deliveries(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, id)): Path<(String, String, TaskActionId)>,
    Query(q): Query<DeliveryListQuery>,
) -> WebResult<Json<BaseResponse<Vec<DeliveryListItem>>>> {
    let (_project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Member,
    )
    .await?;

    ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);

    let rows = ETaskActionDelivery::find()
        .filter(CTaskActionDelivery::ActionId.eq(id))
        .order_by(CTaskActionDelivery::DeliveredAt, Order::Desc)
        .limit(limit)
        .offset(offset)
        .all(&state.web_db)
        .await?;

    Ok(ok_json(
        rows.into_iter().map(to_delivery_list_item).collect(),
    ))
}

pub async fn get_delivery(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task, action_id, delivery_id)): Path<(
        String,
        String,
        TaskActionId,
        TaskActionDeliveryId,
    )>,
) -> WebResult<Json<BaseResponse<DeliveryDetail>>> {
    let (_project, proj) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Member,
    )
    .await?;

    ETaskAction::find()
        .filter(CTaskAction::Id.eq(action_id))
        .filter(CTaskAction::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let r = ETaskActionDelivery::find_by_id(delivery_id)
        .filter(CTaskActionDelivery::ActionId.eq(action_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Delivery")?;

    Ok(ok_json(DeliveryDetail {
        request_body: r.request_body.clone(),
        response_body: r.response_body.clone(),
        item: to_delivery_list_item(r),
    }))
}
