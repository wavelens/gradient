/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD endpoints for `project_action` plus test-fire, token regeneration,
//! and delivery inspection.

use crate::access::{Caller, ProjectAccess, load_project};
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
use gradient_util::http_validation::validate_webhook_url;
use gradient_types::actions::{ActionConfig, ActionType};
use gradient_types::input::load_secret_bytes;
use gradient_types::*;
use gradient_core::ServerState;
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
    pub id: ProjectActionId,
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

fn action_type_to_str(t: ActionType) -> &'static str {
    match t {
        ActionType::SendMail => "send_mail",
        ActionType::SendWebRequest => "send_web_request",
        ActionType::ForgeStatusReport => "forge_status_report",
        ActionType::OpenPr => "open_pr",
    }
}

/// Render a stored row as a public response, stripping the encrypted token
/// from `send_web_request` configs so secrets never leak past the create
/// call where the plaintext is returned exactly once.
fn to_response(m: MProjectAction) -> ActionResponse {
    let at = ActionType::from_i16(m.action_type).unwrap_or(ActionType::SendMail);
    let mut config = m.config;
    if at == ActionType::SendWebRequest
        && let Some(obj) = config.as_object_mut()
    {
        obj.remove("token");
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
        action_type: action_type_to_str(at).into(),
        config,
        events,
        active: m.active,
        last_fired_at: m.last_fired_at,
        created_by: m.created_by,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// `GET /projects/{org}/{project}/actions` - list all actions for the project.
pub async fn list_actions(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<Vec<ActionResponse>>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let rows = EProjectAction::find()
        .filter(CProjectAction::Project.eq(proj.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(rows.into_iter().map(to_response).collect()))
}

/// `POST /projects/{org}/{project}/actions` - create a new action. For
/// `send_web_request` configs the supplied plaintext token is returned
/// exactly once in the response and stored encrypted with the server's
/// crypt key; all later reads omit it entirely.
pub async fn create_action(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<CreateActionRequest>,
) -> WebResult<Json<BaseResponse<CreateActionResponse>>> {
    let (org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    match &body.config {
        ActionConfig::SendMail { .. } => {
            if !state.email.is_enabled() {
                return Err(WebError::unprocessable_entity(
                    "SMTP is not configured on this server",
                ));
            }
        }
        ActionConfig::ForgeStatusReport { .. } => {
            if !body.events.is_empty() {
                return Err(WebError::unprocessable_entity(
                    "forge_status_report actions cannot carry custom events",
                ));
            }
        }
        ActionConfig::SendWebRequest { url, .. } => {
            if let Err(e) = validate_webhook_url(url) {
                return Err(WebError::unprocessable_entity(e.to_string()));
            }
        }
        ActionConfig::OpenPr { .. } => {}
    }

    if let ActionConfig::SendMail { recipients, .. } = &body.config
        && recipients.is_empty()
    {
        return Err(WebError::unprocessable_entity(
            "send_mail requires at least one recipient",
        ));
    }

    let integration_id = match &body.config {
        ActionConfig::ForgeStatusReport { integration_id }
        | ActionConfig::OpenPr { integration_id, .. } => Some(*integration_id),
        _ => None,
    };
    if let Some(integration_id) = integration_id {
        let integration = EIntegration::find()
            .filter(CIntegration::Id.eq(integration_id))
            .filter(CIntegration::Organization.eq(org.id))
            .one(&state.web_db)
            .await?;
        match integration {
            Some(row) if row.kind == i16::from(IntegrationKind::Outbound) => {}
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

    let existing = EProjectAction::find()
        .filter(CProjectAction::Project.eq(proj.id))
        .filter(CProjectAction::Name.eq(body.name.clone()))
        .one(&state.web_db)
        .await?;
    if existing.is_some() {
        return Err(WebError::Conflict(
            crate::error::ErrorCode::ALREADY_EXISTS,
            "action with this name already exists".into(),
        ));
    }

    let (stored_config, plaintext_token) = match body.config.clone() {
        ActionConfig::SendWebRequest {
            url,
            token: Some(plaintext),
        } => {
            let key = load_secret_bytes(&state.config.secrets.crypt_secret_file)
                .map_err(|e| WebError::internal(e.to_string()))?;
            let encrypted = encrypt_action_secret(&plaintext, key.expose())
                .map_err(|e| WebError::internal(e.to_string()))?;
            (
                ActionConfig::SendWebRequest {
                    url,
                    token: Some(encrypted),
                },
                Some(plaintext),
            )
        }
        other => (other, None),
    };

    let now = Utc::now().naive_utc();
    let am = MProjectAction {
        id: ProjectActionId::now_v7(),
        project: proj.id,
        name: body.name,
        action_type: stored_config.action_type().to_i16(),
        config: serde_json::to_value(&stored_config)
            .map_err(|e| WebError::internal(e.to_string()))?,
        events: serde_json::to_value(&body.events).map_err(|e| WebError::internal(e.to_string()))?,
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
    Path((organization, project, id)): Path<(String, String, ProjectActionId)>,
) -> WebResult<Json<BaseResponse<ActionResponse>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let row = EProjectAction::find()
        .filter(CProjectAction::Id.eq(id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    Ok(ok_json(to_response(row)))
}

pub async fn update_action(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectActionId)>,
    Json(body): Json<UpdateActionRequest>,
) -> WebResult<Json<BaseResponse<ActionResponse>>> {
    let (org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let row = EProjectAction::find()
        .filter(CProjectAction::Id.eq(id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let existing_type = ActionType::from_i16(row.action_type).unwrap_or(ActionType::SendMail);

    if let Some(ref new_cfg) = body.config {
        if new_cfg.action_type() != existing_type {
            return Err(WebError::unprocessable_entity(
                "action_type cannot be changed",
            ));
        }
        match new_cfg {
            ActionConfig::SendMail { recipients, .. } if recipients.is_empty() => {
                return Err(WebError::unprocessable_entity(
                    "send_mail requires at least one recipient",
                ));
            }
            ActionConfig::SendWebRequest { url, .. } => {
                if let Err(e) = validate_webhook_url(url) {
                    return Err(WebError::unprocessable_entity(e.to_string()));
                }
            }
            ActionConfig::ForgeStatusReport { integration_id }
            | ActionConfig::OpenPr { integration_id, .. } => {
                let integration = EIntegration::find()
                    .filter(CIntegration::Id.eq(*integration_id))
                    .filter(CIntegration::Organization.eq(org.id))
                    .one(&state.web_db)
                    .await?;
                match integration {
                    Some(r) if r.kind == i16::from(IntegrationKind::Outbound) => {}
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
            ActionType::ForgeStatusReport | ActionType::OpenPr
        )
        && !evs.is_empty()
    {
        return Err(WebError::unprocessable_entity(
            "forge_status_report and open_pr actions cannot carry custom events",
        ));
    }

    let mut active: AProjectAction = row.into();

    if let Some(new_cfg) = body.config {
        // For send_web_request, token: None means preserve the existing encrypted token.
        let stored_cfg = match new_cfg {
            ActionConfig::SendWebRequest { url, token: None } => {
                let existing_config: ActionConfig =
                    serde_json::from_value(active.config.as_ref().clone())
                        .map_err(|e| WebError::internal(e.to_string()))?;
                let existing_token =
                    if let ActionConfig::SendWebRequest { token, .. } = existing_config {
                        token
                    } else {
                        None
                    };
                ActionConfig::SendWebRequest {
                    url,
                    token: existing_token,
                }
            }
            ActionConfig::SendWebRequest {
                url,
                token: Some(plaintext),
            } => {
                let key = load_secret_bytes(&state.config.secrets.crypt_secret_file)
                    .map_err(|e| WebError::internal(e.to_string()))?;
                let encrypted = encrypt_action_secret(&plaintext, key.expose())
                    .map_err(|e| WebError::internal(e.to_string()))?;
                ActionConfig::SendWebRequest {
                    url,
                    token: Some(encrypted),
                }
            }
            other => other,
        };
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

    Ok(ok_json(to_response(updated)))
}

#[derive(Serialize, Debug)]
pub struct DeletedResponse {
    deleted: bool,
}

pub async fn delete_action(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectActionId)>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let row = EProjectAction::find()
        .filter(CProjectAction::Id.eq(id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let active: AProjectAction = row.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json(DeletedResponse { deleted: true }))
}

pub async fn test_action(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectActionId)>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization.clone(),
        project.clone(),
        ProjectAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let action = EProjectAction::find()
        .filter(CProjectAction::Id.eq(id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let action_type = ActionType::from_i16(action.action_type).unwrap_or(ActionType::SendMail);
    let event = match action_type {
        ActionType::ForgeStatusReport | ActionType::OpenPr => "build.completed".to_string(),
        _ => action
            .events
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("evaluation.completed")
            .to_string(),
    };

    let now = chrono::Utc::now();
    let payload = serde_json::json!({
        "synthetic": true,
        "event": event,
        "org": organization,
        "project": project,
        "id": "00000000-0000-0000-0000-000000000000",
        "status": match action_type {
            ActionType::ForgeStatusReport => "success",
            ActionType::OpenPr => "opened",
            _ => "ok",
        },
        "time": now.to_rfc3339(),
        "link": format!("https://gradient.example/projects/{}/{}", organization, project),
        "owner": "gradient-test",
        "repo": project,
        "sha": "0000000000000000000000000000000000000000",
        "context": "gradient/test-fire",
    });

    gradient_ci::actions::execute_action(&state.ci(), action, &event, payload)
        .await
        .map_err(|e| WebError::internal(format!("test fire failed: {}", e)))?;

    Ok(ok_json(serde_json::Value::Null))
}

pub async fn regenerate_token(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectActionId)>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngExt as _;

    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageActions,
            reject_managed: false,
        },
    )
    .await?;

    let existing = EProjectAction::find()
        .filter(CProjectAction::Id.eq(id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    if ActionType::from_i16(existing.action_type) != Some(ActionType::SendWebRequest) {
        return Err(WebError::unprocessable_entity(
            "regenerate-token is only valid for send_web_request actions",
        ));
    }

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    let plaintext_token = format!("gat_{}", URL_SAFE_NO_PAD.encode(raw));

    let key = load_secret_bytes(&state.config.secrets.crypt_secret_file)
        .map_err(|e| WebError::internal(e.to_string()))?;
    let encrypted = encrypt_action_secret(&plaintext_token, key.expose())
        .map_err(|e| WebError::internal(e.to_string()))?;

    let mut cfg: ActionConfig = serde_json::from_value(existing.config.clone())
        .map_err(|e| WebError::internal(e.to_string()))?;
    if let ActionConfig::SendWebRequest { token: t, .. } = &mut cfg {
        *t = Some(encrypted);
    }

    let mut am: AProjectAction = existing.into();
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
    pub id: ProjectActionDeliveryId,
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

fn to_delivery_list_item(r: MProjectActionDelivery) -> DeliveryListItem {
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
    Path((organization, project, id)): Path<(String, String, ProjectActionId)>,
    Query(q): Query<DeliveryListQuery>,
) -> WebResult<Json<BaseResponse<Vec<DeliveryListItem>>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    EProjectAction::find()
        .filter(CProjectAction::Id.eq(id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);

    let rows = EProjectActionDelivery::find()
        .filter(CProjectActionDelivery::ActionId.eq(id))
        .order_by(CProjectActionDelivery::DeliveredAt, Order::Desc)
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
    Path((organization, project, action_id, delivery_id)): Path<(
        String,
        String,
        ProjectActionId,
        ProjectActionDeliveryId,
    )>,
) -> WebResult<Json<BaseResponse<DeliveryDetail>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    EProjectAction::find()
        .filter(CProjectAction::Id.eq(action_id))
        .filter(CProjectAction::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Action")?;

    let r = EProjectActionDelivery::find_by_id(delivery_id)
        .filter(CProjectActionDelivery::ActionId.eq(action_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Delivery")?;

    Ok(ok_json(DeliveryDetail {
        request_body: r.request_body.clone(),
        response_body: r.response_body.clone(),
        item: to_delivery_list_item(r),
    }))
}
