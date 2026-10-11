/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Secrets and access tokens are stored encrypted with the server's crypt key. Responses are
//! exposing only the `has_secret` and `has_access_token` flags.

use super::refusal::ProjectRefusal;
use crate::access::{load_integration_in_project, reject_managed_integration};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::names::IntegrationName;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Requires, RequiresUnmanaged, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_db::lookup::by_id;

use gradient_ci::IntegrationKind;
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_core::ServerState;
use gradient_types::GitHostType;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{IntoActiveModel, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Debug)]
pub struct IntegrationResponse {
    pub id: IntegrationId,
    pub project: ProjectId,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub git_host_type: String,
    pub endpoint_url: Option<String>,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub allowed_ips: Vec<String>,
    pub installation_id: Option<i64>,
    pub account_login: Option<String>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
}

impl From<MIntegration> for IntegrationResponse {
    fn from(m: MIntegration) -> Self {
        Self {
            id: m.id,
            project: m.project,
            name: m.name,
            display_name: m.display_name,
            kind: kind_to_str(m.kind).to_string(),
            git_host_type: m.git_host_type.as_path_segment().to_string(),
            endpoint_url: m.endpoint_url,
            has_secret: m.secret.is_some(),
            has_access_token: m.access_token.is_some(),
            allowed_ips: m.allowed_ips.unwrap_or_default(),
            installation_id: None,
            account_login: None,
            created_by: m.created_by,
            created_at: m.created_at,
            managed: m.managed,
        }
    }
}

async fn integration_response(
    db: &impl sea_orm::ConnectionTrait,
    m: MIntegration,
) -> Result<IntegrationResponse, WebError> {
    let install = match m.github_installation {
        Some(fk) => by_id::<gradient_entity::github_installation::Entity, _>(db, fk).await?,
        None => None,
    };
    Ok(IntegrationResponse {
        installation_id: install.as_ref().map(|i| i.installation_id),
        account_login: install.and_then(|i| i.account_login),
        ..IntegrationResponse::from(m)
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
            SharedRefusal::InvalidAllowedIp {
                entry: e.clone(),
                reason: err.to_string(),
            }
        })?;
        out.push(canon);
    }
    Ok(Some(out))
}

#[derive(Deserialize, Debug)]
pub struct CreateIntegrationRequest {
    pub name: IntegrationName,
    #[serde(default)]
    pub display_name: Option<String>,
    pub kind: String,
    pub git_host_type: String,
    pub secret: Option<String>,
    pub endpoint_url: Option<String>,
    pub access_token: Option<String>,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
    pub installation_id: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct IntegrationSummaryResponse {
    pub id: IntegrationId,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub git_host_type: String,
}

impl From<MIntegration> for IntegrationSummaryResponse {
    fn from(m: MIntegration) -> Self {
        Self {
            id: m.id,
            name: m.name,
            display_name: m.display_name,
            kind: kind_to_str(m.kind).to_string(),
            git_host_type: m.git_host_type.as_path_segment().to_string(),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct PatchIntegrationRequest {
    pub name: Option<IntegrationName>,
    pub display_name: Option<String>,
    pub git_host_type: Option<String>,
    pub endpoint_url: Option<String>,
    pub secret: Option<String>,
    pub access_token: Option<String>,
    pub allowed_ips: Option<Vec<String>>,
}

fn parse_kind(s: &str) -> Result<IntegrationKind, WebError> {
    match s {
        "inbound" => Ok(IntegrationKind::Inbound),
        "outbound" => Ok(IntegrationKind::Outbound),
        other => Err(ProjectRefusal::InvalidIntegrationKind {
            kind: other.to_string(),
        }
        .into()),
    }
}

fn parse_git_host(s: &str) -> Result<GitHostType, WebError> {
    GitHostType::from_path_segment(s).ok_or_else(|| {
        WebError::from(ProjectRefusal::InvalidGitHostType {
            host_type: s.to_string(),
        })
    })
}

fn kind_to_str(k: IntegrationKind) -> &'static str {
    match k {
        IntegrationKind::Inbound => "inbound",
        IntegrationKind::Outbound => "outbound",
    }
}

pub async fn get_integrations(
    ProjectScope { project, db, .. }: ProjectScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<Vec<IntegrationResponse>>>> {
    let rows = gradient_db::integrations::of_project(&db.web_db, project.id).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(integration_response(&db.web_db, row).await?);
    }

    Ok(ok_json(out))
}

pub async fn get_integration_summaries(
    ProjectScope { project, db, .. }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<IntegrationSummaryResponse>>>> {
    let rows = gradient_db::integrations::of_project(&db.web_db, project.id).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(IntegrationSummaryResponse::from)
            .collect(),
    ))
}

pub async fn put_integration(
    ProjectScope { project, user, .. }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<CreateIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let git_host = parse_git_host(&body.git_host_type)?;

    if matches!(git_host, GitHostType::GitHub) {
        let installation_id = body
            .installation_id
            .ok_or(ProjectRefusal::InstallationIdRequired)?;
        let Some(app) = state.config.github_app.clone() else {
            return Err(ProjectRefusal::GitHubAppNotConfigured.into());
        };
        let pem = tokio::fs::read_to_string(&app.private_key_file)
            .await
            .context("read github app key")?;
        let account = gradient_git_host::github_app::get_installation(
            &state.http,
            app.app_id,
            &pem,
            installation_id,
        )
        .await
        .map_err(|e| ProjectRefusal::InvalidInstallationId {
            reason: e.to_string(),
        })?;

        let txn = state.web_db.inner().begin().await?;
        let inst = gradient_ci::upsert_github_installation(
            &txn,
            project.id,
            installation_id,
            Some(&account),
            user.id,
        )
        .await?;
        let name = gradient_ci::github_integration_name(Some(&account), installation_id);
        gradient_ci::ensure_github_app_integrations(
            &txn, project.id, inst, &name, "GitHub", user.id,
        )
        .await?;

        let created = gradient_db::integrations::outbound_of_installation(&txn, project.id, inst)
            .await?
            .ok_or_else(|| ProjectRefusal::IntegrationNameTaken {
                name: name.to_string(),
            })?;
        txn.commit().await?;

        return Ok(ok_json(integration_response(&state.web_db, created).await?));
    }

    let kind = parse_kind(&body.kind)?;

    let existing =
        gradient_db::integrations::by_name(&state.web_db, project.id, kind, body.name.as_str())
            .await?;
    if existing.is_some() {
        return Err(WebError::already_exists(Resource::IntegrationName));
    }

    let encrypted_secret = match body.secret.as_deref() {
        Some(s) if !s.is_empty() => Some(
            encrypt_secret_with_file(&state.config.secrets.crypt_file, s)
                .context("encrypt secret")?,
        ),
        _ => None,
    };

    let encrypted_token = match body.access_token.as_deref() {
        Some(t) if !t.is_empty() => Some(
            encrypt_secret_with_file(&state.config.secrets.crypt_file, t)
                .context("encrypt token")?,
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
        .unwrap_or_else(|| body.name.to_string());

    let allowed_ips = normalize_allowed_ips(body.allowed_ips.clone())?.filter(|v| !v.is_empty());
    let integration = gradient_db::rows::insert::<EIntegration, _>(
        &state.web_db,
        MIntegration {
            id: IntegrationId::now_v7(),
            project: project.id,
            name: body.name.into_inner(),
            display_name,
            kind,
            git_host_type: git_host,
            secret: encrypted_secret,
            endpoint_url,
            access_token: encrypted_token,
            allowed_ips,
            github_installation: None,
            created_by: user.id,
            created_at: gradient_types::now(),
            managed: false,
        },
    )
    .await?;

    Ok(ok_json(
        integration_response(&state.web_db, integration).await?,
    ))
}

pub async fn get_integration(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let integration_id: IntegrationId = params.param("id")?;

    let integration = load_integration_in_project(&db.web_db, project.id, integration_id).await?;
    Ok(ok_json(
        integration_response(&db.web_db, integration).await?,
    ))
}

pub async fn patch_integration(
    ProjectScope {
        project, params, ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<PatchIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let integration_id: IntegrationId = params.param("id")?;

    let integration =
        load_integration_in_project(&state.web_db, project.id, integration_id).await?;
    reject_managed_integration(&integration)?;
    if integration.git_host_type == GitHostType::GitHub {
        return Err(ProjectRefusal::GitHubIntegrationNotEditable.into());
    }
    let kind = integration.kind;

    let mut active: AIntegration = integration.into_active_model();

    if let Some(name) = body.name {
        let clash = gradient_db::integrations::other_by_name(
            &state.web_db,
            project.id,
            kind,
            name.as_str(),
            integration_id,
        )
        .await?;
        if clash.is_some() {
            return Err(WebError::already_exists(Resource::IntegrationName));
        }
        active.name = Set(name.into_inner());
    }

    if let Some(display_name) = body.display_name {
        let trimmed = display_name.trim().to_string();
        if trimmed.is_empty() {
            return Err(SharedRefusal::EmptyField("display_name").into());
        }
        active.display_name = Set(trimmed);
    }

    if let Some(git_host) = body.git_host_type {
        let parsed = parse_git_host(&git_host)?;
        if matches!(parsed, GitHostType::GitHub) {
            return Err(ProjectRefusal::SwitchToGitHubNotAllowed.into());
        }
        active.git_host_type = Set(parsed);
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
                encrypt_secret_with_file(&state.config.secrets.crypt_file, &secret)
                    .context("encrypt secret")?,
            )
        });
    }

    if let Some(token) = body.access_token {
        active.access_token = Set(if token.is_empty() {
            None
        } else {
            Some(
                encrypt_secret_with_file(&state.config.secrets.crypt_file, &token)
                    .context("encrypt token")?,
            )
        });
    }

    if let Some(canon) = normalize_allowed_ips(body.allowed_ips)? {
        active.allowed_ips = Set(if canon.is_empty() { None } else { Some(canon) });
    }

    let updated = gradient_db::rows::update::<EIntegration, _>(&state.web_db, active).await?;

    Ok(ok_json(integration_response(&state.web_db, updated).await?))
}

pub async fn delete_integration(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let integration_id: IntegrationId = params.param("id")?;

    let integration = load_integration_in_project(&db.web_db, project.id, integration_id).await?;
    reject_managed_integration(&integration)?;
    if integration.git_host_type == GitHostType::GitHub {
        let txn = db.web_db.inner().begin().await?;
        match integration.github_installation {
            Some(fk) => {
                gradient_db::integrations::delete_installation_with_integrations(&txn, fk).await?;
            }
            None => {
                gradient_db::rows::delete::<EIntegration, _>(&txn, integration).await?;
            }
        }
        txn.commit().await?;
        return Ok(ok_json(true));
    }
    gradient_db::rows::delete::<EIntegration, _>(&db.web_db, integration).await?;
    Ok(ok_json(true))
}
