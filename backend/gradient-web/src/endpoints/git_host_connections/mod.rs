/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Secrets and access tokens stay encrypted at rest. Responses show only has_secret and has_access_token.

pub mod admin;
pub mod project;
pub mod project_gitea_oauth2_connection;
mod refusal;
pub mod repository_grant_list_create_delete;
mod summaries;
pub mod team;

pub(crate) use refusal::GitHostConnectionRefusal;
pub(crate) use summaries::{
    ConnectionToSummarize, GitHostConnectionReaderScope, SummaryOptions,
    VisibleWebhookDeliveryRepositoryUrlsByGitHostConnection, summaries, summaries_of_owner,
};

use crate::audit::Audit;
use crate::endpoints::tasks::repository::TaskCommitStatusPostAttempt;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::ip_allowlist::normalize_allowed_ips;
use crate::names::GitHostConnectionName;
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use anyhow::Context;
use gradient_ci::CiContext;
use gradient_ci::actions::{encrypt_secret_with_file, new_webhook_secret};
use gradient_db::git_host_connections::{self as connections, GitHostConnectionOwnerByDatabaseId};
use gradient_db::lookup::{by_id, lock_for_admin_change};
use gradient_entity::incoming_webhook_delivery::DeliveryResult;
use gradient_git_host::repository_fit::GitHostConnectionAddress;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::git_host::{
    GitHostConnectionCapabilities, GitHostConnectionOwner, MissingGitHostConnectionSetting,
};
use gradient_types::*;
use gradient_util::http_base_url::{ValidatedHttpBaseUrl, urls_differ_in_scheme_host_or_port};
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionResponse {
    #[serde(flatten)]
    pub connection: GitHostConnectionSummary,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub allowed_ips: Vec<String>,
    pub installation_id: Option<i64>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub generated_secret: Option<String>,
}

#[derive(Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct GitHostConnectionSummary {
    pub id: GitHostConnectionId,
    pub owner: GitHostConnectionOwner,
    pub name: String,
    pub display_name: String,
    pub git_host_type: String,
    pub base_url: Option<String>,
    pub receives_events: bool,
    pub reports_status: bool,
    #[schema(required = false, default = false)]
    pub can_manage: bool,
    #[schema(required = false, default = false)]
    pub managed: bool,
    pub account_login: Option<String>,
    pub github_installation_id: Option<i64>,
    pub webhook_url: Option<String>,
    pub access_token_expires_at: Option<chrono::NaiveDateTime>,
    #[schema(required = false, default = false)]
    pub access_token_refreshes_automatically: bool,
    pub last_delivery: Option<IncomingWebhookDeliveryResponse>,
    #[schema(required = false, default = 0)]
    pub refused_signature_count: i64,
    pub last_refused_signature_at: Option<chrono::NaiveDateTime>,
    pub last_status_post: Option<TaskCommitStatusPostAttempt>,
    #[schema(required = false, default = 0)]
    pub task_count: u64,
    pub repository_fit: Option<RepositoryFit>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
pub struct RepositoryFit {
    pub fits: bool,
    pub can_be_selected: bool,
    pub reason: Option<String>,
}

impl RepositoryFit {
    pub(crate) fn of(repository: &str, connection: &MGitHostConnection) -> Self {
        match check_task_repository_url_can_use_git_host_connection(repository, connection) {
            Err(reason) => Self {
                fits: false,
                can_be_selected: false,
                reason: Some(reason),
            },
            Ok(()) => Self {
                fits: true,
                can_be_selected: true,
                reason: None,
            },
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionTaskResponse {
    pub id: TaskId,
    pub project: String,
    pub name: String,
    pub display_name: String,
    pub repository: String,
    pub report_status: bool,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionTestResponse {
    pub success: bool,
    pub account: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Debug, Clone, utoipa::ToSchema)]
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

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CreateGitHostConnectionRequest {
    pub name: GitHostConnectionName,
    #[serde(default)]
    pub display_name: Option<String>,
    pub git_host_type: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default = "default_true")]
    pub receives_events: bool,
    #[serde(default)]
    pub reports_status: bool,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
    #[serde(default)]
    pub installation_id: Option<i64>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchGitHostConnectionRequest {
    pub name: Option<GitHostConnectionName>,
    pub display_name: Option<String>,
    pub git_host_type: Option<String>,
    pub base_url: Option<String>,
    pub secret: Option<String>,
    pub access_token: Option<String>,
    pub allowed_ips: Option<Vec<String>>,
    pub receives_events: Option<bool>,
    pub reports_status: Option<bool>,
}

pub(crate) const DELIVERY_LIST_LIMIT: u64 = 50;

pub(crate) async fn name_owner(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
) -> WebResult<GitHostConnectionOwner> {
    Ok(match owner {
        GitHostConnectionOwnerByDatabaseId::Instance => GitHostConnectionOwner::Instance,
        GitHostConnectionOwnerByDatabaseId::Team(team) => GitHostConnectionOwner::Team {
            name: by_id::<ETeam, _>(db, team)
                .await?
                .or_not_found(Resource::Team)?
                .name,
        },
        GitHostConnectionOwnerByDatabaseId::Project(project) => GitHostConnectionOwner::Project {
            name: by_id::<EProject, _>(db, project)
                .await?
                .or_not_found(Resource::Project)?
                .name,
        },
    })
}

pub(crate) fn webhook_url(serve_url: &str, connection: &MGitHostConnection) -> Option<String> {
    (connection.receives_events && connection.github_installation_id.is_none()).then(|| {
        format!(
            "{}/api/v1/hooks/{}",
            serve_url.trim_end_matches('/'),
            connection.id
        )
    })
}

pub(crate) async fn response(
    db: &impl sea_orm::ConnectionTrait,
    reader: &GitHostConnectionReaderScope<'_>,
    serve_url: &str,
    connection: MGitHostConnection,
) -> WebResult<GitHostConnectionResponse> {
    let options = SummaryOptions {
        serve_url,
        repository: None,
    };
    let summary = summaries(
        db,
        reader,
        &options,
        vec![ConnectionToSummarize {
            connection: connection.clone(),
            can_manage: true,
        }],
    )
    .await?
    .pop()
    .or_not_found(Resource::GitHostConnection)?;
    Ok(GitHostConnectionResponse {
        connection: summary,
        has_secret: connection.secret.is_some(),
        has_access_token: connection.access_token.is_some(),
        allowed_ips: connection.allowed_ips.unwrap_or_default(),
        installation_id: connection.github_installation_id,
        created_by: connection.created_by,
        created_at: connection.created_at,
        generated_secret: None,
    })
}

pub(crate) async fn insert_and_link_fitting_tasks(
    db: &sea_orm::DatabaseConnection,
    connection: MGitHostConnection,
) -> Result<(MGitHostConnection, Vec<MTask>), sea_orm::DbErr> {
    let txn = db.begin().await?;
    let created = gradient_db::rows::insert::<EGitHostConnection, _>(&txn, connection).await?;
    let linked = link_fitting_tasks_without_connection(&txn, &created).await?;
    txn.commit().await?;
    Ok((created, linked))
}

pub(crate) async fn save_github_app_installation_and_link_tasks_when_new(
    db: &sea_orm::DatabaseConnection,
    installation_id: i64,
    account_login: Option<&str>,
    creator: UserId,
) -> Result<MGitHostConnection, sea_orm::DbErr> {
    let txn = db.begin().await?;
    let saved =
        gradient_ci::github_app_connection::upsert_github_app_installation_git_host_connection(
            &txn,
            installation_id,
            account_login,
            creator,
        )
        .await?;
    if saved.inserted {
        link_fitting_tasks_without_connection(&txn, &saved.connection).await?;
    }
    txn.commit().await?;
    Ok(saved.connection)
}

async fn link_fitting_tasks_without_connection(
    db: &impl sea_orm::ConnectionTrait,
    connection: &MGitHostConnection,
) -> Result<Vec<MTask>, sea_orm::DbErr> {
    let candidates = match GitHostConnectionOwnerByDatabaseId::of(connection) {
        GitHostConnectionOwnerByDatabaseId::Project(project) => {
            gradient_db::tasks::of_project(project).all(db).await?
        }
        GitHostConnectionOwnerByDatabaseId::Instance
            if connection.github_installation_id.is_some()
                && connection.account_login.is_some() =>
        {
            gradient_db::tasks::with_repository_containing(db, "github").await?
        }
        _ => Vec::new(),
    };
    link_fitting_tasks_with_repository_grant(db, connection, candidates).await
}

async fn link_fitting_tasks_with_repository_grant(
    db: &impl sea_orm::ConnectionTrait,
    connection: &MGitHostConnection,
    candidates: Vec<MTask>,
) -> Result<Vec<MTask>, sea_orm::DbErr> {
    let fitting: Vec<MTask> = candidates
        .into_iter()
        .filter(|task| {
            !task.managed
                && task.git_host_connection.is_none()
                && repository_fits_connection_with_known_host(&task.repository, connection)
        })
        .collect();
    let fitting_ids: Vec<TaskId> = fitting.iter().map(|task| task.id).collect();
    let granted =
        gradient_db::git_host_connection_repository_grants::filter_task_ids_by_git_host_connection_repository_grants(
            db,
            connection.id,
            &fitting_ids,
        )
        .await?;
    let (linked, without_grant): (Vec<MTask>, Vec<MTask>) = fitting
        .into_iter()
        .partition(|task| granted.contains(&task.id));
    for task in &without_grant {
        let repository = gradient_types::flake_url::parse_owner_repo(&task.repository)
            .map(|(owner, name)| format!("{owner}/{name}"));
        tracing::info!(
            connection = %connection.id,
            task = %task.id,
            project = %task.project,
            repository = ?repository,
            "Task repositories that match a Git host connection stay unlinked when there is no repository grant for the repository in the task's project."
        );
    }
    let ids: Vec<TaskId> = linked.iter().map(|task| task.id).collect();
    let updated =
        connections::link_tasks_to_connection(db, &ids, connection.id, connection.reports_status)
            .await?;
    Ok(linked
        .into_iter()
        .filter(|task| updated.contains(&task.id))
        .collect())
}

pub(crate) async fn load_of_owner(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
) -> WebResult<MGitHostConnection> {
    connections::by_id_of_owner(db, owner, id)
        .await?
        .or_not_found(Resource::GitHostConnection)
}

fn audit_log_owner_of_git_host_connection(connection: &MGitHostConnection) -> EventOwner {
    match GitHostConnectionOwnerByDatabaseId::of(connection) {
        GitHostConnectionOwnerByDatabaseId::Instance => EventOwner::default(),
        GitHostConnectionOwnerByDatabaseId::Team(team) => EventOwner::team(team),
        GitHostConnectionOwnerByDatabaseId::Project(project) => EventOwner::project(project),
    }
}

async fn write_audit(audit: &Audit, action: Action, connection: &MGitHostConnection) {
    audit
        .write(
            action,
            audit_log_owner_of_git_host_connection(connection),
            serde_json::json!({
                "git_host_connection_id": connection.id.to_string(),
                "name": connection.name,
                "git_host_type": connection.git_host_type,
                "receives_events": connection.receives_events,
                "reports_status": connection.reports_status,
            }),
        )
        .await;
}

fn reject_managed(connection: &MGitHostConnection) -> WebResult<()> {
    if connection.managed {
        return Err(
            SharedRefusal::StateManaged(Change::Modify, Resource::GitHostConnection).into(),
        );
    }
    Ok(())
}

fn parse_git_host(s: &str) -> WebResult<GitHostType> {
    GitHostType::from_path_segment(s).ok_or_else(|| {
        GitHostConnectionRefusal::InvalidGitHostType {
            host_type: s.to_owned(),
        }
        .into()
    })
}

fn http_base_url(url: &str) -> WebResult<String> {
    url.parse::<ValidatedHttpBaseUrl>()
        .map(|base_url| base_url.to_string())
        .map_err(|e| GitHostConnectionRefusal::InvalidBaseUrl(e.to_string()).into())
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn resolve_base_url_for_git_host(
    git_host_type: GitHostType,
    base_url: Option<&str>,
) -> WebResult<Option<String>> {
    gradient_types::git_host::base_url_of_git_host_type_and_base_url(
        git_host_type,
        non_blank(base_url),
    )
    .map(http_base_url)
    .transpose()
}

fn check_capabilities(connection: &MGitHostConnection) -> WebResult<()> {
    if connection.github_installation_id.is_none() && connection.base_url.is_none() {
        return Err(GitHostConnectionRefusal::GitHostConnectionBaseUrlMissing.into());
    }
    GitHostConnectionCapabilities {
        github_app: connection.github_installation_id.is_some(),
        receives_events: connection.receives_events,
        reports_status: connection.reports_status,
        has_base_url: connection.base_url.is_some(),
        has_access_token: connection.access_token.is_some(),
        has_webhook_secret: connection.secret.is_some(),
    }
    .check()
    .map_err(|missing| {
        match missing {
            MissingGitHostConnectionSetting::Capability => GitHostConnectionRefusal::NoCapability,
            MissingGitHostConnectionSetting::BaseUrlOrAccessTokenForStatus => {
                GitHostConnectionRefusal::StatusNeedsBaseUrlAndToken
            }
            MissingGitHostConnectionSetting::WebhookSecretForEvents => {
                GitHostConnectionRefusal::WebhookSecretRequired
            }
        }
        .into()
    })
}

fn encrypt(ci: &CiContext, plain: &str) -> WebResult<String> {
    Ok(
        encrypt_secret_with_file(&ci.db.config.secrets.crypt_file, plain)
            .context("encrypt git host connection credential")?,
    )
}

async fn create_github_app_connection(
    ci: &CiContext,
    owner: GitHostConnectionOwnerByDatabaseId,
    user: UserId,
    installation_id: Option<i64>,
) -> WebResult<MGitHostConnection> {
    if owner != GitHostConnectionOwnerByDatabaseId::Instance {
        return Err(GitHostConnectionRefusal::GitHubNeedsInstanceApp.into());
    }
    let installation_id =
        installation_id.ok_or(GitHostConnectionRefusal::InstallationIdRequired)?;
    let app = ci
        .db
        .config
        .github_app
        .clone()
        .ok_or(GitHostConnectionRefusal::GitHubAppNotConfigured)?;
    let pem = tokio::fs::read_to_string(&app.private_key_file)
        .await
        .context("read github app key")?;
    let account = gradient_git_host::github_app::get_installation(
        &ci.db.user_supplied_url_http_client,
        app.app_id,
        &pem,
        installation_id,
    )
    .await
    .map_err(|e| GitHostConnectionRefusal::InvalidInstallationId {
        reason: e.to_string(),
    })?;
    Ok(save_github_app_installation_and_link_tasks_when_new(
        ci.db.web_db.inner(),
        installation_id,
        Some(&account),
        user,
    )
    .await?)
}

pub(crate) async fn create(
    ci: &CiContext,
    owner: GitHostConnectionOwnerByDatabaseId,
    reader: &GitHostConnectionReaderScope<'_>,
    body: CreateGitHostConnectionRequest,
    audit: &Audit,
) -> WebResult<GitHostConnectionResponse> {
    let user = reader.user.id;
    let git_host = parse_git_host(&body.git_host_type)?;
    let serve_url = &ci.db.config.server.serve_url;
    if git_host == GitHostType::GitHub {
        let created = create_github_app_connection(ci, owner, user, body.installation_id).await?;
        write_audit(audit, Action::GitHostConnectionCreate, &created).await;
        return response(&ci.db.web_db, reader, serve_url, created).await;
    }

    let name = body.name.into_inner();
    if connections::by_name(&ci.db.web_db, owner, &name)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::GitHostConnectionName));
    }
    let generated_secret = (body.receives_events && non_blank(body.secret.as_deref()).is_none())
        .then(new_webhook_secret);
    let secret = non_blank(body.secret.as_deref())
        .or(generated_secret.as_deref())
        .map(|s| encrypt(ci, s))
        .transpose()?;
    let access_token = non_blank(body.access_token.as_deref())
        .map(|t| encrypt(ci, t))
        .transpose()?;
    let (project, owner_team) = match owner {
        GitHostConnectionOwnerByDatabaseId::Instance => (None, None),
        GitHostConnectionOwnerByDatabaseId::Team(team) => (None, Some(team)),
        GitHostConnectionOwnerByDatabaseId::Project(project) => (Some(project), None),
    };
    let connection = MGitHostConnection {
        id: GitHostConnectionId::now_v7(),
        project,
        owner_team,
        display_name: non_blank(body.display_name.as_deref())
            .unwrap_or(&name)
            .to_owned(),
        name,
        git_host_type: git_host,
        secret,
        base_url: resolve_base_url_for_git_host(git_host, body.base_url.as_deref())?,
        access_token,
        encrypted_refresh_token: None,
        access_token_expires_at: None,
        allowed_ips: normalize_allowed_ips(body.allowed_ips)?.filter(|v| !v.is_empty()),
        receives_events: body.receives_events,
        reports_status: body.reports_status,
        github_installation_id: None,
        account_login: None,
        created_by: user,
        created_at: gradient_types::now(),
        managed: false,
        refused_signature_count: 0,
        last_refused_signature_at: None,
    };
    check_capabilities(&connection)?;
    let (created, _) = insert_and_link_fitting_tasks(ci.db.web_db.inner(), connection)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::GitHostConnectionName))?;
    write_audit(audit, Action::GitHostConnectionCreate, &created).await;
    let mut out = response(&ci.db.web_db, reader, serve_url, created).await?;
    out.generated_secret = generated_secret;
    Ok(out)
}

pub(crate) async fn patch(
    ci: &CiContext,
    owner: GitHostConnectionOwnerByDatabaseId,
    reader: &GitHostConnectionReaderScope<'_>,
    id: GitHostConnectionId,
    body: PatchGitHostConnectionRequest,
    audit: &Audit,
) -> WebResult<GitHostConnectionResponse> {
    let txn = ci.db.web_db.inner().begin().await?;
    let current = lock_for_admin_change::<EGitHostConnection, _>(&txn, id)
        .await?
        .filter(|connection| GitHostConnectionOwnerByDatabaseId::of(connection) == owner)
        .or_not_found(Resource::GitHostConnection)?;
    reject_managed(&current)?;
    if current.github_installation_id.is_some() {
        return Err(GitHostConnectionRefusal::GitHubConnectionNotEditable.into());
    }

    let mut next = current.clone();
    if let Some(name) = body.name {
        let name = name.into_inner();
        if connections::other_by_name(&txn, owner, &name, id)
            .await?
            .is_some()
        {
            return Err(WebError::already_exists(Resource::GitHostConnectionName));
        }
        next.name = name;
    }
    if let Some(display_name) = non_blank(body.display_name.as_deref()) {
        next.display_name = display_name.to_owned();
    }
    if let Some(host) = body.git_host_type {
        let host = parse_git_host(&host)?;
        if host == GitHostType::GitHub {
            return Err(GitHostConnectionRefusal::SwitchToGitHubNotAllowed.into());
        }
        next.git_host_type = host;
    }
    if let Some(base_url) = body.base_url {
        next.base_url = resolve_base_url_for_git_host(next.git_host_type, Some(&base_url))?;
    }
    let git_host_target_changed = next.git_host_type != current.git_host_type
        || match (next.base_url.as_deref(), current.base_url.as_deref()) {
            (Some(next), Some(current)) => urls_differ_in_scheme_host_or_port(next, current),
            (next, current) => next.is_some() != current.is_some(),
        };
    if git_host_target_changed {
        next.access_token = None;
        next.encrypted_refresh_token = None;
        next.access_token_expires_at = None;
        next.secret = None;
    }
    if let Some(secret) = body.secret {
        next.secret = non_blank(Some(&secret))
            .map(|s| encrypt(ci, s))
            .transpose()?;
    }
    if let Some(token) = body.access_token {
        next.access_token = non_blank(Some(&token))
            .map(|t| encrypt(ci, t))
            .transpose()?;
        next.encrypted_refresh_token = None;
        next.access_token_expires_at = None;
    }
    if let Some(canon) = normalize_allowed_ips(body.allowed_ips)? {
        next.allowed_ips = (!canon.is_empty()).then_some(canon);
    }
    next.receives_events = body.receives_events.unwrap_or(next.receives_events);
    next.reports_status = body.reports_status.unwrap_or(next.reports_status);
    let generated_secret = (next.receives_events
        && (!current.receives_events || git_host_target_changed)
        && next.secret.is_none())
    .then(new_webhook_secret);
    if let Some(secret) = &generated_secret {
        next.secret = Some(encrypt(ci, secret)?);
    }
    check_capabilities(&next)?;

    let updated = connections::write_all_columns_of_git_host_connection(&txn, next)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::GitHostConnectionName))?;
    txn.commit().await?;
    write_audit(audit, Action::GitHostConnectionUpdate, &updated).await;
    let mut out = response(
        &ci.db.web_db,
        reader,
        &ci.db.config.server.serve_url,
        updated,
    )
    .await?;
    out.generated_secret = generated_secret;
    Ok(out)
}

pub(crate) async fn delete(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
    audit: &Audit,
) -> WebResult<()> {
    let connection = load_of_owner(db, owner, id).await?;
    reject_managed(&connection)?;
    gradient_db::rows::delete::<EGitHostConnection, _>(db, connection.clone()).await?;
    write_audit(audit, Action::GitHostConnectionDelete, &connection).await;
    Ok(())
}

async fn sign_in_account(
    ci: &CiContext,
    connection: &MGitHostConnection,
) -> Result<String, String> {
    if let Some(installation_id) = connection.github_installation_id {
        let app = ci
            .db
            .config
            .github_app
            .clone()
            .ok_or_else(|| GitHostConnectionRefusal::GitHubAppNotConfigured.to_string())?;
        let pem = tokio::fs::read_to_string(&app.private_key_file)
            .await
            .map_err(|_| "Cannot read the private key for the GitHub App".to_owned())?;
        return gradient_git_host::github_app::get_installation(
            &ci.db.user_supplied_url_http_client,
            app.app_id,
            &pem,
            installation_id,
        )
        .await
        .map_err(|e| e.to_string());
    }
    let base_url = read_checked_git_host_connection_base_url_or_host_type_default(connection)
        .map_err(|e| e.to_string())?
        .to_string();
    gradient_util::http_validation::parse_and_check_allowed_outbound_url(
        &base_url,
        ci.db.user_supplied_url_http_client.allowlist(),
    )
    .map_err(|e| format!("Gradient does not call {base_url}: {e}"))?;
    let token = gradient_ci::git_host_connection_token_refresh::refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
        ci.db.web_db.inner(),
        &ci.db.config,
        &ci.db.user_supplied_url_http_client,
        connection,
    )
    .await
    .map_err(|e| format!("Gradient failed to obtain a working access token: {e}"))?
    .ok_or_else(|| "The connection has no access token.".to_owned())?;
    gradient_git_host::provision::token_account_login(
        ci.db.user_supplied_url_http_client.clone(),
        connection.git_host_type,
        &base_url,
        token.expose(),
    )
    .await
    .map_err(|e| e.to_string())
}

pub(crate) async fn test(
    ci: &CiContext,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
) -> WebResult<GitHostConnectionTestResponse> {
    let connection = load_of_owner(&ci.db.web_db, owner, id).await?;
    Ok(match sign_in_account(ci, &connection).await {
        Ok(account) => GitHostConnectionTestResponse {
            success: true,
            account: Some(account),
            error: None,
        },
        Err(error) => GitHostConnectionTestResponse {
            success: false,
            account: None,
            error: Some(error),
        },
    })
}

pub(crate) async fn deliveries(
    db: &impl sea_orm::ConnectionTrait,
    reader: &GitHostConnectionReaderScope<'_>,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
) -> WebResult<Vec<IncomingWebhookDeliveryResponse>> {
    let connection = load_of_owner(db, owner, id).await?;
    deliveries_of_connection(db, reader, &connection).await
}

pub(crate) async fn deliveries_of_connection(
    db: &impl sea_orm::ConnectionTrait,
    reader: &GitHostConnectionReaderScope<'_>,
    connection: &MGitHostConnection,
) -> WebResult<Vec<IncomingWebhookDeliveryResponse>> {
    let limits =
        VisibleWebhookDeliveryRepositoryUrlsByGitHostConnection::of(db, reader, &[connection])
            .await?;
    Ok(
        gradient_db::incoming_deliveries::of_connection(db, connection.id, DELIVERY_LIST_LIMIT)
            .await?
            .into_iter()
            .filter(|delivery| limits.allow(delivery))
            .map(Into::into)
            .collect(),
    )
}

pub(crate) async fn tasks_using_connection_visible_to_reader(
    db: &impl sea_orm::ConnectionTrait,
    connection: GitHostConnectionId,
    reader: &GitHostConnectionReaderScope<'_>,
) -> WebResult<Vec<GitHostConnectionTaskResponse>> {
    let tasks = connections::tasks_using_connection(db, connection).await?;
    let visible_project_names = summaries::project_names_visible_to_reader(
        db,
        reader,
        tasks.iter().map(|task| task.project),
    )
    .await?;
    Ok(tasks
        .into_iter()
        .filter_map(|task| {
            Some(GitHostConnectionTaskResponse {
                project: visible_project_names.get(&task.project)?.clone(),
                id: task.id,
                name: task.name,
                display_name: task.display_name,
                repository: task.repository,
                report_status: task.report_status,
            })
        })
        .collect())
}

fn address_of(connection: &MGitHostConnection) -> GitHostConnectionAddress<'_> {
    GitHostConnectionAddress {
        git_host_type: connection.git_host_type,
        base_url: connection.base_url.as_deref(),
        github_app_account: connection
            .github_installation_id
            .and(connection.account_login.as_deref()),
    }
}

pub(crate) fn repository_fits_connection_with_known_host(
    repository: &str,
    connection: &MGitHostConnection,
) -> bool {
    RepositoryFit::of(repository, connection).fits
}

pub(crate) fn read_checked_git_host_connection_base_url_or_host_type_default(
    connection: &MGitHostConnection,
) -> Result<ValidatedHttpBaseUrl, GitHostConnectionRefusal> {
    gradient_types::git_host::base_url_of_git_host_connection(connection)
        .ok_or(GitHostConnectionRefusal::GitHostConnectionBaseUrlMissing)?
        .parse::<ValidatedHttpBaseUrl>()
        .map_err(|e| GitHostConnectionRefusal::InvalidBaseUrl(e.to_string()))
}

pub(crate) fn check_task_repository_url_can_use_git_host_connection(
    repository: &str,
    connection: &MGitHostConnection,
) -> Result<(), String> {
    if connection.git_host_type != GitHostType::GitHub || connection.base_url.is_some() {
        read_checked_git_host_connection_base_url_or_host_type_default(connection)
            .map_err(|e| e.to_string())?;
    }
    address_of(connection).check_repository(repository)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(git_host_type: GitHostType, base_url: Option<&str>) -> MGitHostConnection {
        MGitHostConnection {
            git_host_type,
            base_url: base_url.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn a_repository_on_the_connection_host_fits() {
        let gitea = connection(GitHostType::Gitea, Some("https://git.example.com"));
        for repo in [
            "https://git.example.com/acme/infra",
            "git@git.example.com:acme/infra.git",
            "git+https://git.example.com/acme/infra?dir=nix",
        ] {
            assert_eq!(
                check_task_repository_url_can_use_git_host_connection(repo, &gitea),
                Ok(()),
                "{repo}"
            );
        }
        let github = connection(GitHostType::GitHub, None);
        assert_eq!(
            check_task_repository_url_can_use_git_host_connection("github:acme/infra", &github),
            Ok(())
        );
    }

    #[test]
    fn a_repository_on_another_host_is_refused_with_both_hosts() {
        let github = connection(GitHostType::GitHub, None);
        let reason = check_task_repository_url_can_use_git_host_connection(
            "https://gitlab.com/a/b",
            &github,
        )
        .unwrap_err();
        assert!(
            reason.contains("gitlab.com") && reason.contains("github.com"),
            "{reason}"
        );
    }

    #[test]
    fn a_github_app_installation_fits_only_repositories_of_its_account() {
        let installation = MGitHostConnection {
            git_host_type: GitHostType::GitHub,
            github_installation_id: Some(7),
            account_login: Some("Acme".into()),
            ..Default::default()
        };
        assert_eq!(
            check_task_repository_url_can_use_git_host_connection(
                "https://github.com/acme/infra",
                &installation
            ),
            Ok(())
        );
        let reason = check_task_repository_url_can_use_git_host_connection(
            "https://github.com/other/infra",
            &installation,
        )
        .unwrap_err();
        assert!(
            reason.contains("Acme") && reason.contains("other"),
            "{reason}"
        );
    }

    #[test]
    fn only_a_connection_with_a_known_matching_host_fits_without_a_user_choice() {
        let unknown_host = connection(GitHostType::Forgejo, None);
        for repo in [
            "https://github.com/acme/infra",
            "https://git.example.com/acme/infra",
        ] {
            assert!(
                check_task_repository_url_can_use_git_host_connection(repo, &unknown_host).is_err()
            );
            assert!(
                !repository_fits_connection_with_known_host(repo, &unknown_host),
                "{repo}"
            );
        }
        let gitea = connection(GitHostType::Gitea, Some("https://git.example.com"));
        assert!(repository_fits_connection_with_known_host(
            "https://git.example.com/acme/infra",
            &gitea
        ));
        assert!(repository_fits_connection_with_known_host(
            "github:acme/infra",
            &connection(GitHostType::GitHub, None)
        ));
    }

    #[test]
    fn connection_without_base_url_does_not_fit_and_cannot_be_selected() {
        let fit = RepositoryFit::of(
            "https://git.example.com/acme/infra",
            &connection(GitHostType::Forgejo, None),
        );
        assert!(!fit.fits);
        assert!(!fit.can_be_selected);
        assert!(
            fit.reason
                .as_deref()
                .is_some_and(|reason| reason.contains("base_url")),
            "{fit:?}"
        );
        let other_host = RepositoryFit::of(
            "https://github.com/acme/infra",
            &connection(GitHostType::Gitea, Some("https://git.example.com")),
        );
        assert!(!other_host.fits && !other_host.can_be_selected);
    }

    #[test]
    fn a_repository_without_owner_and_name_is_refused() {
        let gitea = connection(GitHostType::Gitea, Some("https://git.example.com"));
        let reason = check_task_repository_url_can_use_git_host_connection(
            "https://git.example.com",
            &gitea,
        )
        .unwrap_err();
        assert!(reason.contains("owner and repository"), "{reason}");
        assert!(
            check_task_repository_url_can_use_git_host_connection("not a url", &gitea).is_err()
        );
    }

    #[test]
    fn only_connections_receiving_events_outside_the_github_app_have_a_webhook_url() {
        let mut row = MGitHostConnection {
            id: GitHostConnectionId::nil(),
            receives_events: true,
            ..Default::default()
        };
        assert_eq!(
            webhook_url("https://ci.example.com/", &row).as_deref(),
            Some("https://ci.example.com/api/v1/hooks/00000000-0000-0000-0000-000000000000")
        );
        row.github_installation_id = Some(1);
        assert_eq!(webhook_url("https://ci.example.com", &row), None);
        row.github_installation_id = None;
        row.receives_events = false;
        assert_eq!(webhook_url("https://ci.example.com", &row), None);
    }

    #[tokio::test]
    async fn linking_fitting_tasks_to_existing_instance_wide_github_app_connection_links_only_task_of_project_with_repository_grant_and_creates_no_grant()
     {
        use gradient_fixtures::rows;
        let test_db = gradient_fixtures::database::TestDatabase::create().await;
        let db = test_db.connection();
        gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&db, rows::user())
            .await
            .unwrap();
        let granted_project = rows::project();
        let other_project = gradient_entity::project::Model {
            id: ProjectId::now_v7(),
            name: "other-project".into(),
            ..rows::project()
        };
        let mut task_ids = Vec::new();
        for project in [&granted_project, &other_project] {
            gradient_db::rows::insert::<gradient_entity::project::Entity, _>(&db, project.clone())
                .await
                .unwrap();
            let task = gradient_db::rows::insert::<ETask, _>(
                &db,
                MTask {
                    id: TaskId::now_v7(),
                    project: project.id,
                    repository: "https://github.com/acme/infra".into(),
                    ..rows::task()
                },
            )
            .await
            .unwrap();
            task_ids.push(task.id);
        }
        let connection = gradient_db::rows::insert::<EGitHostConnection, _>(
            &db,
            MGitHostConnection {
                id: GitHostConnectionId::now_v7(),
                name: "github-acme".into(),
                display_name: "GitHub".into(),
                git_host_type: GitHostType::GitHub,
                receives_events: true,
                reports_status: true,
                github_installation_id: Some(9999),
                account_login: Some("acme".into()),
                created_by: gradient_fixtures::ids::user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        gradient_db::git_host_connection_repository_grants::insert(
            &db,
            gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant {
                connection: connection.id,
                project: granted_project.id,
                repository_owner: "acme",
                repository_name: Some("infra"),
                source: gradient_entity::git_host_connection_repository_grant::RepositoryGrantSource::Admin,
                created_by: None,
            },
        )
        .await
        .unwrap();

        let linked = link_fitting_tasks_without_connection(&db, &connection)
            .await
            .unwrap();

        assert_eq!(
            linked.iter().map(|task| task.id).collect::<Vec<_>>(),
            [task_ids[0]]
        );
        let tasks = test_db.rows::<ETask>().await;
        let connection_of = |id: TaskId| {
            tasks
                .iter()
                .find(|task| task.id == id)
                .unwrap()
                .git_host_connection
        };
        assert_eq!(connection_of(task_ids[0]), Some(connection.id));
        assert_eq!(connection_of(task_ids[1]), None);
        assert_eq!(
            test_db
                .rows::<gradient_entity::git_host_connection_repository_grant::Entity>()
                .await
                .len(),
            1
        );
        test_db.finish().await;
    }
}
