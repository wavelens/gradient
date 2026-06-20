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
//! and never returned in responses - responses only expose a boolean
//! "has_secret" / "has_access_token" flag.

use crate::access::{Caller, OrgAccess, load_integration_in_org, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};

use gradient_ci::actions::encrypt_secret_with_file;
use gradient_ci::IntegrationKind;
use gradient_types::ForgeType;
use gradient_types::input::check_index_name;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, TransactionTrait};
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
    pub allowed_ips: Vec<String>,
    pub installation_id: Option<i64>,
    pub account_login: Option<String>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
}

fn base_from(m: MIntegration) -> IntegrationResponse {
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
        allowed_ips: m.allowed_ips.unwrap_or_default(),
        installation_id: None,
        account_login: None,
        created_by: m.created_by,
        created_at: m.created_at,
    }
}

async fn integration_response(
    db: &impl sea_orm::ConnectionTrait,
    m: MIntegration,
) -> Result<IntegrationResponse, WebError> {
    let install = match m.github_installation {
        Some(fk) => gradient_entity::github_installation::Entity::find_by_id(fk).one(db).await?,
        None => None,
    };
    Ok(IntegrationResponse {
        installation_id: install.as_ref().map(|i| i.installation_id),
        account_login: install.and_then(|i| i.account_login),
        ..base_from(m)
    })
}

fn normalize_allowed_ips(raw: Option<Vec<String>>) -> Result<Option<Vec<String>>, WebError> {
    let Some(entries) = raw else { return Ok(None) };
    if entries.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let canon = crate::ip_allowlist::normalize_entry(&e).map_err(|err| {
            WebError::bad_request_with(
                crate::error::ErrorCode::INVALID_ALLOWED_IP,
                format!("invalid allowed_ips entry '{e}': {err}"),
            )
        })?;
        out.push(canon);
    }
    Ok(Some(out))
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
    /// CIDR strings; only inbound webhooks from these sources are accepted.
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
    /// Required for `forge_type=github`: the App installation id to bind.
    pub installation_id: Option<i64>,
}

/// Credential-free integration handle. Returned by the summaries endpoint
/// (`GET /orgs/{org}/integrations/summary`) so non-admin org members can
/// render integration names in the trigger UI without learning whether a
/// secret/token is stored or what endpoint URL is configured.
#[derive(Serialize, Debug)]
pub struct IntegrationSummaryResponse {
    pub id: IntegrationId,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub forge_type: String,
}

impl From<MIntegration> for IntegrationSummaryResponse {
    fn from(m: MIntegration) -> Self {
        Self {
            id: m.id,
            name: m.name,
            display_name: m.display_name,
            kind: kind_to_str(m.kind).to_string(),
            forge_type: forge_to_str(m.forge_type).to_string(),
        }
    }
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
    /// Wholesale replacement; `[]` clears the allowlist.
    pub allowed_ips: Option<Vec<String>>,
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
    ForgeType::try_from(f)
        .map(ForgeType::as_path_segment)
        .unwrap_or("unknown")
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /orgs/{organization}/integrations` - list integrations for an org.
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

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(integration_response(&state.web_db, row).await?);
    }

    Ok(ok_json(out))
}

/// `GET /orgs/{organization}/integrations/summary` - list integrations as
/// credential-free summaries. Available to any org member; the full listing
/// remains gated on `ManageIntegrations` because it exposes `has_secret`,
/// `has_access_token`, and `endpoint_url`. Used by the trigger UI to render
/// integration names and populate the create/edit dropdown for users with
/// `EditProject` who do not also hold `ManageIntegrations`.
pub async fn get_integration_summaries(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<IntegrationSummaryResponse>>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let rows = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(
        rows.into_iter()
            .map(IntegrationSummaryResponse::from)
            .collect(),
    ))
}

/// `PUT /orgs/{organization}/integrations` - create a new integration.
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

    let forge = parse_forge(&body.forge_type)?;

    if matches!(forge, ForgeType::GitHub) {
        let installation_id = body.installation_id.ok_or_else(|| {
            WebError::bad_request("forge_type 'github' requires an installation_id")
        })?;
        let Some(app) = state.config.github_app.clone() else {
            return Err(WebError::bad_request(
                "GitHub App is not configured on this server; set GRADIENT_GITHUB_APP_* first",
            ));
        };
        let pem = tokio::fs::read_to_string(&app.private_key_file)
            .await
            .map_err(|e| WebError::internal(format!("reading github app key: {e}")))?;
        let account = gradient_forge::github_app::get_installation(
            &state.http, app.app_id, &pem, installation_id,
        )
        .await
        .map_err(|e| WebError::bad_request(format!("invalid installation_id: {e}")))?;

        let txn = state.web_db.inner().begin().await?;
        let inst = gradient_ci::upsert_github_installation(
            &txn, org.id, installation_id, Some(&account), user.id,
        )
        .await?;
        let name = gradient_ci::github_integration_name(Some(&account), installation_id);
        gradient_ci::ensure_github_app_integrations(
            &txn, org.id, inst, &name, "GitHub", user.id,
        )
        .await?;

        let created = EIntegration::find()
            .filter(CIntegration::Organization.eq(org.id))
            .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Outbound)))
            .filter(CIntegration::GithubInstallation.eq(inst))
            .one(&txn)
            .await?
            .ok_or_else(|| {
                WebError::conflict(format!(
                    "an integration named '{name}' already exists in this organization; \
                     rename it before binding this installation"
                ))
            })?;
        txn.commit().await?;

        return Ok(ok_json(integration_response(&state.web_db, created).await?));
    }

    let kind = parse_kind(&body.kind)?;

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
            encrypt_secret_with_file(&state.config.secrets.crypt_secret_file, s)
                .map_err(|e| WebError::internal(format!("Failed to encrypt secret: {}", e)))?,
        ),
        _ => None,
    };

    let encrypted_token = match body.access_token.as_deref() {
        Some(t) if !t.is_empty() => Some(
            encrypt_secret_with_file(&state.config.secrets.crypt_secret_file, t)
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

    let allowed_ips = normalize_allowed_ips(body.allowed_ips.clone())?
        .and_then(|v| if v.is_empty() { None } else { Some(v) });
    let integration = MIntegration {
        id: IntegrationId::now_v7(),
        organization: org.id,
        name: body.name,
        display_name,
        kind: i16::from(kind),
        forge_type: i16::from(forge),
        secret: encrypted_secret,
        endpoint_url,
        access_token: encrypted_token,
        allowed_ips,
        github_installation: None,
        created_by: user.id,
        created_at: gradient_types::now(),
    }
    .into_active_model();

    let integration = integration.insert(&state.web_db).await?;

    Ok(ok_json(integration_response(&state.web_db, integration).await?))
}

/// `GET /orgs/{organization}/integrations/{id}` - fetch a single integration.
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
    Ok(ok_json(integration_response(&state.web_db, integration).await?))
}

/// `PATCH /orgs/{organization}/integrations/{id}` - update an integration.
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
                encrypt_secret_with_file(&state.config.secrets.crypt_secret_file, &secret)
                    .map_err(|e| WebError::internal(format!("Failed to encrypt secret: {}", e)))?,
            )
        });
    }

    if let Some(token) = body.access_token {
        active.access_token = Set(if token.is_empty() {
            None
        } else {
            Some(
                encrypt_secret_with_file(&state.config.secrets.crypt_secret_file, &token)
                    .map_err(|e| WebError::internal(format!("Failed to encrypt token: {}", e)))?,
            )
        });
    }

    if let Some(canon) = normalize_allowed_ips(body.allowed_ips)? {
        active.allowed_ips = Set(if canon.is_empty() { None } else { Some(canon) });
    }

    let updated = active.update(&state.web_db).await?;

    Ok(ok_json(integration_response(&state.web_db, updated).await?))
}

/// `DELETE /orgs/{organization}/integrations/{id}` - remove an integration.
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
        let txn = state.web_db.inner().begin().await?;
        match integration.github_installation {
            Some(fk) => {
                EIntegration::delete_many()
                    .filter(CIntegration::GithubInstallation.eq(fk))
                    .exec(&txn)
                    .await?;
                gradient_entity::github_installation::Entity::delete_by_id(fk)
                    .exec(&txn)
                    .await?;
            }
            None => {
                integration.into_active_model().delete(&txn).await?;
            }
        }
        txn.commit().await?;
        return Ok(ok_json(true));
    }
    integration
        .into_active_model()
        .delete(&state.web_db)
        .await?;
    Ok(ok_json(true))
}
