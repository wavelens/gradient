/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Secrets and access tokens are stored encrypted with the server's crypt key. Responses are
//! exposing only the `has_secret` and `has_access_token` flags.

use super::refusal::ProjectRefusal;
use crate::access::{load_integration_in_project, reject_managed_integration};
use crate::api_spec::ProjectPath;
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
use gradient_entity::incoming_webhook_delivery::DeliveryResult;

use gradient_ci::IntegrationKind;
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_core::ServerState;
use gradient_types::GitHostType;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{IntoActiveModel, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Debug, utoipa::ToSchema)]
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
    pub generated_secret: Option<String>,
    pub webhook_url: Option<String>,
    pub connected_tasks: Vec<String>,
}

impl IntegrationResponse {
    fn new(
        integration: MIntegration,
        installation: Option<gradient_entity::github_installation::Model>,
        webhook_url: Option<String>,
    ) -> Self {
        Self {
            id: integration.id,
            project: integration.project,
            name: integration.name,
            display_name: integration.display_name,
            kind: kind_to_str(integration.kind).to_string(),
            git_host_type: integration.git_host_type.as_path_segment().to_string(),
            endpoint_url: integration.endpoint_url,
            has_secret: integration.secret.is_some(),
            has_access_token: integration.access_token.is_some(),
            allowed_ips: integration.allowed_ips.unwrap_or_default(),
            installation_id: installation.as_ref().map(|i| i.installation_id),
            account_login: installation.and_then(|i| i.account_login),
            created_by: integration.created_by,
            created_at: integration.created_at,
            managed: integration.managed,
            generated_secret: None,
            webhook_url,
            connected_tasks: Vec::new(),
        }
    }
}

pub(crate) struct WebhookBase<'a> {
    pub serve_url: &'a str,
    pub project: &'a str,
}

impl WebhookBase<'_> {
    pub(crate) fn url_for(&self, integration: &MIntegration) -> Option<String> {
        let per_integration = integration.kind == IntegrationKind::Inbound
            && integration.git_host_type != GitHostType::GitHub;
        per_integration.then(|| {
            format!(
                "{}/api/v1/hooks/{}/{}/{}",
                self.serve_url.trim_end_matches('/'),
                integration.git_host_type.as_path_segment(),
                self.project,
                integration.name
            )
        })
    }
}

async fn integration_response(
    db: &impl sea_orm::ConnectionTrait,
    base: &WebhookBase<'_>,
    m: MIntegration,
) -> Result<IntegrationResponse, WebError> {
    let install = match m.github_installation {
        Some(fk) => by_id::<gradient_entity::github_installation::Entity, _>(db, fk).await?,
        None => None,
    };
    let webhook_url = base.url_for(&m);
    Ok(IntegrationResponse::new(m, install, webhook_url))
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

#[derive(Deserialize, Debug, utoipa::ToSchema)]
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

#[derive(Serialize, Debug, utoipa::ToSchema)]
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

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct ConnectGitHostRequest {
    pub name: IntegrationName,
    #[serde(default)]
    pub display_name: Option<String>,
    pub git_host_type: String,
    pub endpoint_url: String,
    #[serde(default)]
    pub access_token: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ConnectGitHostResponse {
    pub inbound: IntegrationResponse,
    pub outbound: Option<IntegrationResponse>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct IncomingWebhookDeliveryResponse {
    pub id: IncomingWebhookDeliveryId,
    pub event: String,
    pub status_code: i16,
    pub result: DeliveryResult,
    pub detail: Option<String>,
    pub repository: Option<String>,
    pub tasks_scanned: i32,
    pub queued: i32,
    pub created_at: chrono::NaiveDateTime,
}

impl From<gradient_entity::incoming_webhook_delivery::Model> for IncomingWebhookDeliveryResponse {
    fn from(m: gradient_entity::incoming_webhook_delivery::Model) -> Self {
        Self {
            id: m.id,
            event: m.event,
            status_code: m.status_code,
            result: m.result,
            detail: m.detail,
            repository: m.repository,
            tasks_scanned: m.tasks_scanned,
            queued: m.queued,
            created_at: m.created_at,
        }
    }
}

const DELIVERY_LIST_LIMIT: u64 = 50;

#[derive(Deserialize, Debug, utoipa::ToSchema)]
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

#[utoipa::path(
    get,
    path = "/projects/{project}/integrations",
    operation_id = "listProjectIntegrations",
    tag = "integrations",
    summary = "List integrations",
    description = "List all inbound and outbound integrations in this project.",
    params(ProjectPath),
    responses((status = OK, description = "List of integrations", body = Vec<IntegrationResponse>), WebError)
)]
pub async fn get_integrations(
    ProjectScope { project, db, .. }: ProjectScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<Vec<IntegrationResponse>>>> {
    let rows = gradient_db::integrations::of_project(&db.web_db, project.id).await?;
    let base = WebhookBase {
        serve_url: &db.config.server.serve_url,
        project: &project.name,
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(integration_response(&db.web_db, &base, row).await?);
    }

    Ok(ok_json(out))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/integrations/summary",
    operation_id = "listProjectIntegrationSummaries",
    tag = "integrations",
    summary = "List integration summaries (credential-free)",
    description = "List integrations as credential-free summaries. Available to **any\nproject member**, in contrast to the full listing which requires\n`ManageIntegrations`.\n\nThe response excludes `secret`, `endpoint_url`, `access_token`, and\nthe `has_secret` / `has_access_token` booleans so non-admin members\ncannot fingerprint credential state. Intended for UIs that only need\nan integration's name and Git host type - e.g. populating the trigger\ncreate/edit dropdown for users who hold `EditTask` but not\n`ManageIntegrations`.",
    params(ProjectPath),
    responses((status = OK, description = "List of integration summaries", body = Vec<IntegrationSummaryResponse>), WebError)
)]
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

#[utoipa::path(
    put,
    path = "/projects/{project}/integrations",
    operation_id = "createProjectIntegration",
    tag = "integrations",
    summary = "Create integration",
    description = "Creates a named inbound or outbound integration. `secret` and\n`access_token` are stored encrypted and never returned in responses.\nName is unique within `(project, kind)`.\n\nCreating an inbound integration without `secret` makes the server generate one. The create response returns it once in `generated_secret`. Later responses only show `has_secret`.\n\n``webhook_url`` is the address to enter on the Git host. The public server URL and the stored ``git_host_type`` form its base. The same integration can also use ``/hooks/gitea/...``, ``/hooks/forgejo/...`` and ``/hooks/gitlab/...`` as path segments for other self-hosted hosts.\n\n`git_host_type: \"github\"` integrations are creatable by supplying\n`installation_id`. The server App must be configured (else `400`);\nthe installation id is validated against the App (invalid id returns\n`400`). An inbound and outbound integration pair is created\nautomatically, named `github-<account>`. Multiple installations per project\nare supported (one per GitHub account).",
    params(ProjectPath),
    request_body = CreateIntegrationRequest,
    responses((status = OK, description = "Integration created", body = IntegrationResponse), WebError)
)]
pub async fn put_integration(
    ProjectScope { project, user, .. }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<CreateIntegrationRequest>,
) -> WebResult<Json<BaseResponse<IntegrationResponse>>> {
    let git_host = parse_git_host(&body.git_host_type)?;
    let base = WebhookBase {
        serve_url: &state.config.server.serve_url,
        project: &project.name,
    };

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
        let link = gradient_ci::link_project_github_installation(
            &txn,
            project.id,
            installation_id,
            Some(&account),
            user.id,
        )
        .await?;
        let created = gradient_db::integrations::outbound_of_installation(
            &txn,
            project.id,
            link.installation,
        )
        .await?
        .ok_or(ProjectRefusal::IntegrationNameTaken {
            name: link.integration_name,
        })?;
        txn.commit().await?;

        let mut response = integration_response(&state.web_db, &base, created).await?;
        if link.created_any_integration {
            response.connected_tasks = connect_tasks(&state.web_db, project.id).await?;
        }
        return Ok(ok_json(response));
    }

    let kind = parse_kind(&body.kind)?;
    let secret = body
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let access_token = body
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let endpoint_url = body
        .endpoint_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let endpoint_url = match (kind, endpoint_url) {
        (IntegrationKind::Outbound, None) => return Err(ProjectRefusal::InvalidEndpointUrl.into()),
        (_, Some(url)) => Some(http_endpoint_url(url)?),
        (_, None) => None,
    };
    if kind == IntegrationKind::Outbound && access_token.is_none() {
        return Err(SharedRefusal::EmptyField("access_token").into());
    }

    let display_name = body
        .display_name
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| body.name.to_string());
    let allowed_ips = normalize_allowed_ips(body.allowed_ips.clone())?.filter(|v| !v.is_empty());

    let created = create_integrations_and_attach_matching_tasks(
        &state.web_db,
        &state.config.secrets.crypt_file,
        &base,
        SharedIntegrationCreationFields {
            project: project.id,
            name: body.name.into_inner(),
            display_name,
            git_host,
            endpoint_url,
            created_by: user.id,
        },
        vec![IntegrationToCreate {
            kind,
            secret,
            access_token,
            allowed_ips,
        }],
    )
    .await?;
    let response = created
        .into_iter()
        .next()
        .context("The integration insert returned no rows.")?;
    Ok(ok_json(response))
}

struct SharedIntegrationCreationFields {
    project: ProjectId,
    name: String,
    display_name: String,
    git_host: GitHostType,
    endpoint_url: Option<String>,
    created_by: UserId,
}

struct IntegrationToCreate<'a> {
    kind: IntegrationKind,
    secret: Option<&'a str>,
    access_token: Option<&'a str>,
    allowed_ips: Option<Vec<String>>,
}

async fn create_integrations_and_attach_matching_tasks(
    db: &gradient_db::WebDb,
    crypt_file: &str,
    base: &WebhookBase<'_>,
    shared: SharedIntegrationCreationFields,
    rows: Vec<IntegrationToCreate<'_>>,
) -> WebResult<Vec<IntegrationResponse>> {
    for row in &rows {
        if gradient_db::integrations::by_name(db, shared.project, row.kind, &shared.name)
            .await?
            .is_some()
        {
            return Err(WebError::already_exists(Resource::IntegrationName));
        }
    }

    let mut prepared = Vec::with_capacity(rows.len());
    for row in rows {
        let generated_secret =
            (row.kind == IntegrationKind::Inbound && row.secret.is_none()).then(new_webhook_secret);
        let secret = match row.secret.or(generated_secret.as_deref()) {
            Some(s) => Some(encrypt_secret_with_file(crypt_file, s).context("encrypt secret")?),
            None => None,
        };
        let access_token = match row.access_token {
            Some(t) => Some(encrypt_secret_with_file(crypt_file, t).context("encrypt token")?),
            None => None,
        };
        let integration = MIntegration {
            id: IntegrationId::now_v7(),
            project: shared.project,
            name: shared.name.clone(),
            display_name: shared.display_name.clone(),
            kind: row.kind,
            git_host_type: shared.git_host,
            secret,
            endpoint_url: shared.endpoint_url.clone(),
            access_token,
            allowed_ips: row.allowed_ips,
            github_installation: None,
            created_by: shared.created_by,
            created_at: gradient_types::now(),
            managed: false,
        };
        prepared.push((integration, generated_secret));
    }

    let txn = db.inner().begin().await?;
    let mut created = Vec::with_capacity(prepared.len());
    for (integration, generated_secret) in prepared {
        let integration = gradient_db::rows::insert::<EIntegration, _>(&txn, integration)
            .await
            .map_err(|e| WebError::from_db_err(e, Resource::IntegrationName))?;
        created.push((integration, generated_secret));
    }
    txn.commit().await?;

    let connected_tasks = connect_tasks(db, shared.project).await?;
    let mut responses = Vec::with_capacity(created.len());
    for (integration, generated_secret) in created {
        let mut response = integration_response(db, base, integration).await?;
        response.generated_secret = generated_secret;
        response.connected_tasks = connected_tasks.clone();
        responses.push(response);
    }
    Ok(responses)
}

async fn connect_tasks(db: &gradient_db::WebDb, project: ProjectId) -> WebResult<Vec<String>> {
    let tasks = gradient_db::tasks::of_project(project).all(db).await?;
    let integrations = gradient_db::integrations::of_project(db, project).await?;
    let mut connected = Vec::new();
    for task in tasks.iter().filter(|t| !t.managed) {
        match crate::endpoints::tasks::auto_attach::apply(db, task, &integrations).await {
            Ok(attached) if attached.any() => connected.push(task.name.clone()),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(task = %task.id, error = %e, "Could not connect the task to the new integration");
            }
        }
    }
    Ok(connected)
}

fn http_endpoint_url(url: &str) -> WebResult<String> {
    url::Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|_| url.to_owned())
        .ok_or_else(|| ProjectRefusal::InvalidEndpointUrl.into())
}

fn new_webhook_secret() -> String {
    use rand::RngExt as _;

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    hex::encode(raw)
}

#[utoipa::path(
    post,
    path = "/projects/{project}/integrations/connect",
    operation_id = "connectGitHost",
    tag = "integrations",
    summary = "Connect a Git host",
    description = "Create a webhook receiver for a Gitea, Forgejo, or GitLab host and a status reporter with `access_token` present, in one call. Both integrations share `name`. Fields `inbound.generated_secret` and `inbound.webhook_url` in the response contain the generated webhook secret once and the address to enter into the Git host. Existing tasks with repositories on this Git host connect to the new integration. Their task names appear in `connected_tasks`.",
    params(ProjectPath),
    request_body = ConnectGitHostRequest,
    responses((status = OK, description = "Created Integration", body = ConnectGitHostResponse), WebError)
)]
pub async fn post_connect_git_host(
    ProjectScope { project, user, .. }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<ConnectGitHostRequest>,
) -> WebResult<Json<BaseResponse<ConnectGitHostResponse>>> {
    let git_host = parse_git_host(&body.git_host_type)?;
    if git_host == GitHostType::GitHub {
        return Err(ProjectRefusal::GitHubNeedsApp.into());
    }
    let endpoint_url = http_endpoint_url(body.endpoint_url.trim())?;
    let name = body.name.into_inner();
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&name)
        .to_string();
    let token = body
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());

    let mut rows = vec![IntegrationToCreate {
        kind: IntegrationKind::Inbound,
        secret: None,
        access_token: None,
        allowed_ips: None,
    }];
    if let Some(token) = token {
        rows.push(IntegrationToCreate {
            kind: IntegrationKind::Outbound,
            secret: None,
            access_token: Some(token),
            allowed_ips: None,
        });
    }
    let base = WebhookBase {
        serve_url: &state.config.server.serve_url,
        project: &project.name,
    };
    let mut created = create_integrations_and_attach_matching_tasks(
        &state.web_db,
        &state.config.secrets.crypt_file,
        &base,
        SharedIntegrationCreationFields {
            project: project.id,
            name,
            display_name,
            git_host,
            endpoint_url: Some(endpoint_url),
            created_by: user.id,
        },
        rows,
    )
    .await?
    .into_iter();
    let inbound = created
        .next()
        .context("The integration insert returned no rows.")?;
    let outbound = created.next();
    Ok(ok_json(ConnectGitHostResponse { inbound, outbound }))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/integrations/{id}",
    operation_id = "getProjectIntegration",
    tag = "integrations",
    summary = "Get integration",
    params(ProjectPath, ("id" = IntegrationId, Path)),
    responses((status = OK, description = "Integration", body = IntegrationResponse), WebError)
)]
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
    let base = WebhookBase {
        serve_url: &db.config.server.serve_url,
        project: &project.name,
    };
    Ok(ok_json(
        integration_response(&db.web_db, &base, integration).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/integrations/{id}/deliveries",
    operation_id = "listIntegrationDeliveries",
    tag = "integrations",
    summary = "List incoming webhook deliveries",
    description = "List the latest webhook deliveries that reach an inbound integration, newest first. The HTTP status that Gradient returned and the result appear in every entry. Results cover a queued evaluation, a skipped task, no task for the repository, an ignored event, or a refusal with a reason. Only the latest 50 deliveries for each integration stay in Gradient. All project members can access these deliveries.",
    params(ProjectPath, ("id" = IntegrationId, Path)),
    responses((status = OK, description = "Delivery, newest first", body = Vec<IncomingWebhookDeliveryResponse>), WebError)
)]
pub async fn get_integration_deliveries(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<IncomingWebhookDeliveryResponse>>>> {
    let integration_id: IntegrationId = params.param("id")?;
    load_integration_in_project(&db.web_db, project.id, integration_id).await?;
    let rows = gradient_db::incoming_deliveries::of_integration(
        &db.web_db,
        integration_id,
        DELIVERY_LIST_LIMIT,
    )
    .await?;
    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    patch,
    path = "/projects/{project}/integrations/{id}",
    operation_id = "patchProjectIntegration",
    tag = "integrations",
    summary = "Update integration",
    description = "Patch-updates an integration. Omit a field to leave it unchanged.\nPass an empty string for `access_token` to **clear** it. Inbound integrations reject an empty `secret` with `400`.\n\n`git_host_type=github` rows are managed by the App and installation; PATCH\nrejects them with `400`. Delete the row to remove the integration.\nState-managed integrations are rejected with `403`.",
    params(ProjectPath, ("id" = IntegrationId, Path)),
    request_body = PatchIntegrationRequest,
    responses((status = OK, description = "Updated integration", body = IntegrationResponse), WebError)
)]
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
        let trimmed = url.trim();
        active.endpoint_url = Set(if trimmed.is_empty() {
            None
        } else {
            Some(http_endpoint_url(trimmed)?)
        });
    }

    if let Some(secret) = body.secret {
        let secret = secret.trim();
        if secret.is_empty() && kind == IntegrationKind::Inbound {
            return Err(ProjectRefusal::WebhookSecretRequired.into());
        }
        active.secret = Set(if secret.is_empty() {
            None
        } else {
            Some(
                encrypt_secret_with_file(&state.config.secrets.crypt_file, secret)
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

    let updated = gradient_db::rows::update::<EIntegration, _>(&state.web_db, active)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::IntegrationName))?;

    let base = WebhookBase {
        serve_url: &state.config.server.serve_url,
        project: &project.name,
    };
    Ok(ok_json(
        integration_response(&state.web_db, &base, updated).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/integrations/{id}",
    operation_id = "deleteProjectIntegration",
    tag = "integrations",
    summary = "Delete integration",
    description = "Deletes an integration. Any task `task_integration` rows referencing it are cleared via `ON DELETE SET NULL`. `git_host_type=github` rows can be deleted; deleting removes the installation binding for that account. State-managed integrations are rejected with `403`, other integrations of a state-managed project can be deleted.",
    params(ProjectPath, ("id" = IntegrationId, Path)),
    responses((status = OK, description = "Deleted", body = bool, content_type = "application/json"), WebError)
)]
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
