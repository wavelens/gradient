/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD for per-organization named integrations.
//!
//! An integration stores credentials + metadata for a forge (Gitea/Forgejo/
//! GitLab/GitHub). Each row is either **inbound** (the forge calls us;
//! `secret` holds the HMAC secret) or **outbound** (we call the forge;
//! `endpoint_url` + `access_token` hold API credentials).
//!
//! Secrets and access tokens are stored encrypted with the server's crypt key
//! and never returned in responses — responses only expose a boolean
//! "has_secret" / "has_access_token" flag.

use super::{load_editable_org, load_org_member};
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::Utc;
use core::ci::{ForgeType, IntegrationKind, encrypt_webhook_secret};
use core::types::input::check_index_name;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ── Request / response shapes ─────────────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct IntegrationResponse {
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub kind: String,
    pub forge_type: String,
    pub endpoint_url: Option<String>,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub created_by: Uuid,
    pub created_at: chrono::NaiveDateTime,
}

impl From<MIntegration> for IntegrationResponse {
    fn from(m: MIntegration) -> Self {
        IntegrationResponse {
            id: m.id,
            organization: m.organization,
            name: m.name,
            kind: kind_to_str(m.kind).to_string(),
            forge_type: forge_to_str(m.forge_type).to_string(),
            endpoint_url: m.endpoint_url,
            has_secret: m.secret.is_some(),
            has_access_token: m.access_token.is_some(),
            created_by: m.created_by,
            created_at: m.created_at,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct CreateIntegrationRequest {
    pub name: String,
    /// `"inbound"` or `"outbound"`.
    pub kind: String,
    /// `"gitea"`, `"forgejo"`, `"gitlab"`, or `"github"`.
    pub forge_type: String,
    /// Plaintext HMAC secret for inbound integrations.
    pub secret: Option<String>,
    /// Base URL (e.g. `https://gitea.example.com`) for outbound integrations.
    pub endpoint_url: Option<String>,
    /// Plaintext API token for outbound integrations.
    pub access_token: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct PatchIntegrationRequest {
    pub name: Option<String>,
    pub forge_type: Option<String>,
    pub endpoint_url: Option<String>,
    /// When present, replaces the stored secret. Empty string clears it.
    pub secret: Option<String>,
    /// When present, replaces the stored access token. Empty string clears it.
    pub access_token: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct PatchGithubAppRequest {
    pub enabled: bool,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn parse_kind(s: &str) -> Result<IntegrationKind, WebError> {
    match s {
        "inbound" => Ok(IntegrationKind::Inbound),
        "outbound" => Ok(IntegrationKind::Outbound),
        other => Err(WebError::BadRequest(format!(
            "Invalid integration kind '{}': expected 'inbound' or 'outbound'.",
            other
        ))),
    }
}

fn parse_forge(s: &str) -> Result<ForgeType, WebError> {
    ForgeType::from_path_segment(s).ok_or_else(|| {
        WebError::BadRequest(format!(
            "Invalid forge type '{}': expected 'gitea', 'forgejo', 'gitlab', or 'github'.",
            s
        ))
    })
}

fn kind_to_str(k: i16) -> &'static str {
    match k {
        0 => "inbound",
        1 => "outbound",
        _ => "unknown",
    }
}

fn forge_to_str(f: i16) -> &'static str {
    match ForgeType::from_i16(f) {
        Some(ForgeType::Gitea) => "gitea",
        Some(ForgeType::Forgejo) => "forgejo",
        Some(ForgeType::GitLab) => "gitlab",
        Some(ForgeType::GitHub) => "github",
        None => "unknown",
    }
}

async fn load_integration(
    state: &Arc<ServerState>,
    org_id: Uuid,
    integration_id: Uuid,
) -> WebResult<MIntegration> {
    EIntegration::find()
        .filter(CIntegration::Id.eq(integration_id))
        .filter(CIntegration::Organization.eq(org_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Integration"))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /orgs/{organization}/integrations` — list integrations for an org.
pub async fn get_integrations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<IntegrationResponse>>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let rows = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .all(&state.db)
        .await?;

    Ok(Json(BaseResponse {
        error: false,
        message: rows.into_iter().map(IntegrationResponse::from).collect(),
    }))
}

/// `PUT /orgs/{organization}/integrations` — create a new integration.
pub async fn put_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<CreateIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let org = load_editable_org(&state, user.id, organization).await?;

    if check_index_name(&body.name).is_err() {
        return Err(WebError::invalid_name("Integration Name"));
    }

    let kind = parse_kind(&body.kind)?;
    let forge = parse_forge(&body.forge_type)?;

    // Name must be unique within (organization, kind).
    let existing = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .filter(CIntegration::Kind.eq(kind.as_i16()))
        .filter(CIntegration::Name.eq(body.name.as_str()))
        .one(&state.db)
        .await?;
    if existing.is_some() {
        return Err(WebError::already_exists("Integration Name"));
    }

    let encrypted_secret = match body.secret.as_deref() {
        Some(s) if !s.is_empty() => Some(
            encrypt_webhook_secret(&state.cli.crypt_secret_file, s).map_err(|e| {
                WebError::InternalServerError(format!("Failed to encrypt secret: {}", e))
            })?,
        ),
        _ => None,
    };

    let encrypted_token = match body.access_token.as_deref() {
        Some(t) if !t.is_empty() => Some(
            encrypt_webhook_secret(&state.cli.crypt_secret_file, t).map_err(|e| {
                WebError::InternalServerError(format!("Failed to encrypt token: {}", e))
            })?,
        ),
        _ => None,
    };

    let endpoint_url = body.endpoint_url.and_then(|s| {
        let t = s.trim().to_string();
        if t.is_empty() { None } else { Some(t) }
    });

    let integration = AIntegration {
        id: Set(Uuid::new_v4()),
        organization: Set(org.id),
        name: Set(body.name),
        kind: Set(kind.as_i16()),
        forge_type: Set(forge.as_i16()),
        secret: Set(encrypted_secret),
        endpoint_url: Set(endpoint_url),
        access_token: Set(encrypted_token),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let integration = integration.insert(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: IntegrationResponse::from(integration),
    }))
}

/// `GET /orgs/{organization}/integrations/{id}` — fetch a single integration.
pub async fn get_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, integration_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let org = load_org_member(&state, user.id, organization).await?;
    let integration = load_integration(&state, org.id, integration_id).await?;
    Ok(Json(BaseResponse {
        error: false,
        message: IntegrationResponse::from(integration),
    }))
}

/// `PATCH /orgs/{organization}/integrations/{id}` — update an integration.
pub async fn patch_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, integration_id)): Path<(String, Uuid)>,
    Json(body): Json<PatchIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let org = load_editable_org(&state, user.id, organization).await?;
    let integration = load_integration(&state, org.id, integration_id).await?;
    let kind = integration.kind;

    let mut active: AIntegration = integration.into_active_model();

    if let Some(name) = body.name {
        if check_index_name(&name).is_err() {
            return Err(WebError::invalid_name("Integration Name"));
        }
        let clash = EIntegration::find()
            .filter(CIntegration::Organization.eq(org.id))
            .filter(CIntegration::Kind.eq(kind))
            .filter(CIntegration::Name.eq(name.as_str()))
            .filter(CIntegration::Id.ne(integration_id))
            .one(&state.db)
            .await?;
        if clash.is_some() {
            return Err(WebError::already_exists("Integration Name"));
        }
        active.name = Set(name);
    }

    if let Some(forge) = body.forge_type {
        active.forge_type = Set(parse_forge(&forge)?.as_i16());
    }

    if let Some(url) = body.endpoint_url {
        let trimmed = url.trim().to_string();
        active.endpoint_url = Set(if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        });
    }

    if let Some(secret) = body.secret {
        active.secret = Set(if secret.is_empty() {
            None
        } else {
            Some(
                encrypt_webhook_secret(&state.cli.crypt_secret_file, &secret).map_err(|e| {
                    WebError::InternalServerError(format!("Failed to encrypt secret: {}", e))
                })?,
            )
        });
    }

    if let Some(token) = body.access_token {
        active.access_token = Set(if token.is_empty() {
            None
        } else {
            Some(
                encrypt_webhook_secret(&state.cli.crypt_secret_file, &token).map_err(|e| {
                    WebError::InternalServerError(format!("Failed to encrypt token: {}", e))
                })?,
            )
        });
    }

    let updated = active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: IntegrationResponse::from(updated),
    }))
}

/// `DELETE /orgs/{organization}/integrations/{id}` — remove an integration.
pub async fn delete_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, integration_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let org = load_editable_org(&state, user.id, organization).await?;
    let integration = load_integration(&state, org.id, integration_id).await?;
    integration.into_active_model().delete(&state.db).await?;
    Ok(Json(BaseResponse {
        error: false,
        message: true,
    }))
}

/// `PATCH /orgs/{organization}/github-app` — toggle the per-org GitHub App
/// opt-in flag. Rejected when the server has no GitHub App configured.
pub async fn patch_github_app(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<PatchGithubAppRequest>,
) -> WebResult<Json<BaseResponse<bool>>> {
    if state.cli.github_app_config().is_none() {
        return Err(WebError::BadRequest(
            "This server has no GitHub App configured.".to_string(),
        ));
    }

    let org = load_editable_org(&state, user.id, organization).await?;
    let mut active: AOrganization = org.into();
    active.github_app_enabled = Set(body.enabled);
    active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: body.enabled,
    }))
}
