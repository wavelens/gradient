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

use crate::access::{Caller, OrgAccess, load_integration_in_org, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};

use gradient_core::ci::{
    ForgeType, GITHUB_APP_INTEGRATION_NAME, IntegrationKind, encrypt_webhook_secret,
};
use gradient_core::types::input::check_index_name;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Request / response shapes ─────────────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct IntegrationResponse {
    pub id: IntegrationId,
    pub organization: OrganizationId,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub forge_type: String,
    pub endpoint_url: Option<String>,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
}

impl From<MIntegration> for IntegrationResponse {
    fn from(m: MIntegration) -> Self {
        IntegrationResponse {
            id: m.id,
            organization: m.organization,
            name: m.name,
            display_name: m.display_name,
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
    /// Human-readable display name. Defaults to `name` when omitted.
    #[serde(default)]
    pub display_name: Option<String>,
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
    pub display_name: Option<String>,
    pub forge_type: Option<String>,
    pub endpoint_url: Option<String>,
    /// When present, replaces the stored secret. Empty string clears it.
    pub secret: Option<String>,
    /// When present, replaces the stored access token. Empty string clears it.
    pub access_token: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn parse_kind(s: &str) -> Result<IntegrationKind, WebError> {
    match s {
        "inbound" => Ok(IntegrationKind::Inbound),
        "outbound" => Ok(IntegrationKind::Outbound),
        other => Err(WebError::bad_request(format!(
            "Invalid integration kind '{}': expected 'inbound' or 'outbound'.",
            other
        ))),
    }
}

fn parse_forge(s: &str) -> Result<ForgeType, WebError> {
    ForgeType::from_path_segment(s).ok_or_else(|| {
        WebError::bad_request(format!(
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
    match ForgeType::try_from(f) {
        Ok(ForgeType::Gitea) => "gitea",
        Ok(ForgeType::Forgejo) => "forgejo",
        Ok(ForgeType::GitLab) => "gitlab",
        Ok(ForgeType::GitHub) => "github",
        Err(_) => "unknown",
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /orgs/{organization}/integrations` — list integrations for an org.
pub async fn get_integrations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<IntegrationResponse>>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageIntegrations,
            reject_managed: false,
        },
    )
    .await?;

    let rows = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(
        rows.into_iter().map(IntegrationResponse::from).collect(),
    ))
}

/// `PUT /orgs/{organization}/integrations` — create a new integration.
pub async fn put_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
    Json(body): Json<CreateIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageIntegrations,
            reject_managed: true,
        },
    )
    .await?;

    if check_index_name(&body.name).is_err() {
        return Err(WebError::invalid_name("Integration Name"));
    }
    if body.name == GITHUB_APP_INTEGRATION_NAME {
        return Err(WebError::bad_request(format!(
            "Integration name '{}' is reserved for the auto-managed GitHub App row.",
            GITHUB_APP_INTEGRATION_NAME
        )));
    }

    let kind = parse_kind(&body.kind)?;
    let forge = parse_forge(&body.forge_type)?;
    if matches!(forge, ForgeType::GitHub) {
        return Err(WebError::bad_request(
            "GitHub integrations are managed through the server-wide GitHub App; \
             enable the App on the organization instead of creating an integration row.",
        ));
    }

    // Name must be unique within (organization, kind).
    let existing = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .filter(CIntegration::Kind.eq(i16::from(kind)))
        .filter(CIntegration::Name.eq(body.name.as_str()))
        .one(&state.web_db)
        .await?;
    if existing.is_some() {
        return Err(WebError::already_exists("Integration Name"));
    }

    let encrypted_secret = match body.secret.as_deref() {
        Some(s) if !s.is_empty() => Some(
            encrypt_webhook_secret(&state.config.secrets.crypt_secret_file, s)
                .map_err(|e| WebError::internal(format!("Failed to encrypt secret: {}", e)))?,
        ),
        _ => None,
    };

    let encrypted_token = match body.access_token.as_deref() {
        Some(t) if !t.is_empty() => Some(
            encrypt_webhook_secret(&state.config.secrets.crypt_secret_file, t)
                .map_err(|e| WebError::internal(format!("Failed to encrypt token: {}", e)))?,
        ),
        _ => None,
    };

    let endpoint_url = body.endpoint_url.and_then(|s| {
        let t = s.trim().to_string();
        if t.is_empty() { None } else { Some(t) }
    });

    let display_name = body
        .display_name
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| body.name.clone());

    let integration = AIntegration {
        id: Set(IntegrationId::now_v7()),
        organization: Set(org.id),
        name: Set(body.name),
        display_name: Set(display_name),
        kind: Set(i16::from(kind)),
        forge_type: Set(i16::from(forge)),
        secret: Set(encrypted_secret),
        endpoint_url: Set(endpoint_url),
        access_token: Set(encrypted_token),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
    };

    let integration = integration.insert(&state.web_db).await?;

    Ok(ok_json(IntegrationResponse::from(integration)))
}

/// `GET /orgs/{organization}/integrations/{id}` — fetch a single integration.
pub async fn get_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, integration_id)): Path<(String, IntegrationId)>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageIntegrations,
            reject_managed: false,
        },
    )
    .await?;
    let integration = load_integration_in_org(&state, org.id, integration_id).await?;
    Ok(ok_json(IntegrationResponse::from(integration)))
}

/// `PATCH /orgs/{organization}/integrations/{id}` — update an integration.
pub async fn patch_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, integration_id)): Path<(String, IntegrationId)>,
    Json(body): Json<PatchIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageIntegrations,
            reject_managed: true,
        },
    )
    .await?;
    let integration = load_integration_in_org(&state, org.id, integration_id).await?;
    if integration.forge_type == i16::from(ForgeType::GitHub) {
        return Err(WebError::bad_request(
            "GitHub App integrations are managed automatically and cannot be edited.",
        ));
    }
    let kind = integration.kind;

    let mut active: AIntegration = integration.into_active_model();

    if let Some(name) = body.name {
        if check_index_name(&name).is_err() {
            return Err(WebError::invalid_name("Integration Name"));
        }
        if name == GITHUB_APP_INTEGRATION_NAME {
            return Err(WebError::bad_request(format!(
                "Integration name '{}' is reserved for the auto-managed GitHub App row.",
                GITHUB_APP_INTEGRATION_NAME
            )));
        }
        let clash = EIntegration::find()
            .filter(CIntegration::Organization.eq(org.id))
            .filter(CIntegration::Kind.eq(kind))
            .filter(CIntegration::Name.eq(name.as_str()))
            .filter(CIntegration::Id.ne(integration_id))
            .one(&state.web_db)
            .await?;
        if clash.is_some() {
            return Err(WebError::already_exists("Integration Name"));
        }
        active.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let trimmed = display_name.trim().to_string();
        if trimmed.is_empty() {
            return Err(WebError::bad_request(
                "display_name cannot be empty".to_string(),
            ));
        }
        active.display_name = Set(trimmed);
    }

    if let Some(forge) = body.forge_type {
        let parsed = parse_forge(&forge)?;
        if matches!(parsed, ForgeType::GitHub) {
            return Err(WebError::bad_request(
                "GitHub integrations are managed through the server-wide GitHub App; \
                 enable the App on the organization instead of switching forge_type to github.",
            ));
        }
        active.forge_type = Set(i16::from(parsed));
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
                encrypt_webhook_secret(&state.config.secrets.crypt_secret_file, &secret)
                    .map_err(|e| WebError::internal(format!("Failed to encrypt secret: {}", e)))?,
            )
        });
    }

    if let Some(token) = body.access_token {
        active.access_token = Set(if token.is_empty() {
            None
        } else {
            Some(
                encrypt_webhook_secret(&state.config.secrets.crypt_secret_file, &token)
                    .map_err(|e| WebError::internal(format!("Failed to encrypt token: {}", e)))?,
            )
        });
    }

    let updated = active.update(&state.web_db).await?;

    Ok(ok_json(IntegrationResponse::from(updated)))
}

/// `DELETE /orgs/{organization}/integrations/{id}` — remove an integration.
pub async fn delete_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, integration_id)): Path<(String, IntegrationId)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageIntegrations,
            reject_managed: true,
        },
    )
    .await?;
    let integration = load_integration_in_org(&state, org.id, integration_id).await?;
    if integration.forge_type == i16::from(ForgeType::GitHub) {
        return Err(WebError::bad_request(
            "GitHub App integrations are managed automatically and cannot be deleted.",
        ));
    }
    integration
        .into_active_model()
        .delete(&state.web_db)
        .await?;
    Ok(ok_json(true))
}
