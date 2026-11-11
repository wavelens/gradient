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
pub mod team;

pub(crate) use refusal::GitHostConnectionRefusal;

use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::names::GitHostConnectionName;
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use anyhow::Context;
use gradient_ci::CiContext;
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_db::git_host_connections::{self as connections, GitHostConnectionOwnerByDatabaseId};
use gradient_db::lookup::by_id;
use gradient_entity::incoming_webhook_delivery::DeliveryResult;
use gradient_types::git_host::GitHostConnectionOwner;
use gradient_types::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionResponse {
    pub id: GitHostConnectionId,
    pub owner: GitHostConnectionOwner,
    pub name: String,
    pub display_name: String,
    pub git_host_type: String,
    pub base_url: Option<String>,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub access_token_refreshes_automatically: bool,
    pub allowed_ips: Vec<String>,
    pub receives_events: bool,
    pub reports_status: bool,
    pub installation_id: Option<i64>,
    pub account_login: Option<String>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub webhook_url: Option<String>,
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
    pub can_manage: bool,
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

pub(crate) fn summary(
    connection: &MGitHostConnection,
    owner: GitHostConnectionOwner,
    can_manage: bool,
) -> GitHostConnectionSummary {
    GitHostConnectionSummary {
        id: connection.id,
        owner,
        name: connection.name.clone(),
        display_name: connection.display_name.clone(),
        git_host_type: connection.git_host_type.as_path_segment().to_owned(),
        base_url: connection.base_url.clone(),
        receives_events: connection.receives_events,
        reports_status: connection.reports_status,
        can_manage,
    }
}

pub(crate) async fn summaries_of_owner(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    can_manage: bool,
) -> WebResult<Vec<GitHostConnectionSummary>> {
    let named = name_owner(db, owner).await?;
    Ok(connections::of_owner(db, owner)
        .await?
        .iter()
        .map(|connection| summary(connection, named.clone(), can_manage))
        .collect())
}

pub(crate) async fn response(
    db: &impl sea_orm::ConnectionTrait,
    serve_url: &str,
    connection: MGitHostConnection,
) -> WebResult<GitHostConnectionResponse> {
    let owner = name_owner(db, GitHostConnectionOwnerByDatabaseId::of(&connection)).await?;
    Ok(GitHostConnectionResponse {
        id: connection.id,
        owner,
        webhook_url: webhook_url(serve_url, &connection),
        name: connection.name,
        display_name: connection.display_name,
        git_host_type: connection.git_host_type.as_path_segment().to_owned(),
        base_url: connection.base_url,
        has_secret: connection.secret.is_some(),
        has_access_token: connection.access_token.is_some(),
        access_token_refreshes_automatically: connection.encrypted_refresh_token.is_some(),
        allowed_ips: connection.allowed_ips.unwrap_or_default(),
        receives_events: connection.receives_events,
        reports_status: connection.reports_status,
        installation_id: connection.github_installation_id,
        account_login: connection.account_login,
        created_by: connection.created_by,
        created_at: connection.created_at,
        managed: connection.managed,
        generated_secret: None,
    })
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
    url::Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|_| url.trim_end_matches('/').to_owned())
        .ok_or_else(|| GitHostConnectionRefusal::InvalidBaseUrl.into())
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn normalize_allowed_ips(raw: Option<Vec<String>>) -> WebResult<Option<Vec<String>>> {
    let Some(entries) = raw else { return Ok(None) };
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

fn new_webhook_secret() -> String {
    use rand::RngExt as _;

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    hex::encode(raw)
}

fn check_capabilities(connection: &MGitHostConnection) -> WebResult<()> {
    if !connection.receives_events && !connection.reports_status {
        return Err(GitHostConnectionRefusal::NoCapability.into());
    }
    if connection.github_installation_id.is_none()
        && connection.reports_status
        && (connection.base_url.is_none() || connection.access_token.is_none())
    {
        return Err(GitHostConnectionRefusal::StatusNeedsBaseUrlAndToken.into());
    }
    if connection.github_installation_id.is_none()
        && connection.receives_events
        && connection.secret.is_none()
    {
        return Err(GitHostConnectionRefusal::WebhookSecretRequired.into());
    }
    Ok(())
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
        &ci.http,
        app.app_id,
        &pem,
        installation_id,
    )
    .await
    .map_err(|e| GitHostConnectionRefusal::InvalidInstallationId {
        reason: e.to_string(),
    })?;
    Ok(
        gradient_ci::github_app_connection::upsert_github_app_installation_git_host_connection(
            ci.db.web_db.inner(),
            installation_id,
            Some(&account),
            user,
        )
        .await?,
    )
}

pub(crate) async fn create(
    ci: &CiContext,
    owner: GitHostConnectionOwnerByDatabaseId,
    user: UserId,
    body: CreateGitHostConnectionRequest,
) -> WebResult<GitHostConnectionResponse> {
    let git_host = parse_git_host(&body.git_host_type)?;
    let serve_url = &ci.db.config.server.serve_url;
    if git_host == GitHostType::GitHub {
        let created = create_github_app_connection(ci, owner, user, body.installation_id).await?;
        return response(&ci.db.web_db, serve_url, created).await;
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
        base_url: non_blank(body.base_url.as_deref())
            .map(http_base_url)
            .transpose()?,
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
    };
    check_capabilities(&connection)?;
    let created = gradient_db::rows::insert::<EGitHostConnection, _>(&ci.db.web_db, connection)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::GitHostConnectionName))?;
    let mut out = response(&ci.db.web_db, serve_url, created).await?;
    out.generated_secret = generated_secret;
    Ok(out)
}

pub(crate) async fn patch(
    ci: &CiContext,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
    body: PatchGitHostConnectionRequest,
) -> WebResult<GitHostConnectionResponse> {
    let current = load_of_owner(&ci.db.web_db, owner, id).await?;
    reject_managed(&current)?;
    if current.github_installation_id.is_some() {
        return Err(GitHostConnectionRefusal::GitHubConnectionNotEditable.into());
    }

    let mut next = current.clone();
    if let Some(name) = body.name {
        let name = name.into_inner();
        if connections::other_by_name(&ci.db.web_db, owner, &name, id)
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
        next.base_url = non_blank(Some(&base_url)).map(http_base_url).transpose()?;
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
    let generated_secret =
        (next.receives_events && !current.receives_events && next.secret.is_none())
            .then(new_webhook_secret);
    if let Some(secret) = &generated_secret {
        next.secret = Some(encrypt(ci, secret)?);
    }
    check_capabilities(&next)?;

    let updated = connections::write_all_columns_of_git_host_connection(&ci.db.web_db, next)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::GitHostConnectionName))?;
    let mut out = response(&ci.db.web_db, &ci.db.config.server.serve_url, updated).await?;
    out.generated_secret = generated_secret;
    Ok(out)
}

pub(crate) async fn delete(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
) -> WebResult<()> {
    let connection = load_of_owner(db, owner, id).await?;
    reject_managed(&connection)?;
    gradient_db::rows::delete::<EGitHostConnection, _>(db, connection).await?;
    Ok(())
}

pub(crate) async fn deliveries(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
) -> WebResult<Vec<IncomingWebhookDeliveryResponse>> {
    load_of_owner(db, owner, id).await?;
    Ok(
        gradient_db::incoming_deliveries::of_connection(db, id, DELIVERY_LIST_LIMIT)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    )
}

pub(crate) fn url_host(url: &str) -> Option<String> {
    let s = url.trim();
    let s = s.strip_prefix("git+").unwrap_or(s);

    let authority = match ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find_map(|scheme| s.strip_prefix(scheme))
    {
        Some(rest) => rest.split(['/', '?', '#']).next().unwrap_or(""),
        None => s.split_once(':')?.0,
    };
    if authority.contains('/') {
        return None;
    }
    let host_and_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_and_port.split(':').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn flake_shorthand_host(repository: &str) -> Option<&'static str> {
    let lower = repository.trim().to_ascii_lowercase();
    let lower = lower.strip_prefix("git+").unwrap_or(&lower);
    if lower.starts_with("github:") {
        Some("github.com")
    } else if lower.starts_with("gitlab:") {
        Some("gitlab.com")
    } else {
        None
    }
}

fn expected_host(connection: &MGitHostConnection) -> Option<String> {
    match (&connection.base_url, connection.git_host_type) {
        (Some(base_url), _) => url_host(base_url),
        (None, GitHostType::GitHub) => Some("github.com".to_owned()),
        (None, GitHostType::GitLab) => Some("gitlab.com".to_owned()),
        (None, GitHostType::Gitea | GitHostType::Forgejo) => None,
    }
}

pub(crate) fn check_task_repository_url_can_use_git_host_connection(
    repository: &str,
    connection: &MGitHostConnection,
) -> Result<(), String> {
    let host = flake_shorthand_host(repository)
        .map(str::to_owned)
        .or_else(|| url_host(repository))
        .ok_or_else(|| format!("Cannot read the host of {repository}"))?;
    if let Some(expected) = expected_host(connection)
        && expected != host
    {
        return Err(format!(
            "Repository host {host} and connection host {expected} differ"
        ));
    }
    if gradient_ci::parse_owner_repo(repository).is_none() {
        return Err(format!("Can't read owner and repository from {repository}"));
    }
    Ok(())
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
    fn host_parsing_covers_url_shapes() {
        assert_eq!(
            url_host("https://github.com/foo/bar").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            url_host("git@gitea.example.com:foo/bar.git").as_deref(),
            Some("gitea.example.com")
        );
        assert_eq!(
            url_host("ssh://git@gitlab.com:2222/foo/bar").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(url_host("not a url"), None);
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
}
