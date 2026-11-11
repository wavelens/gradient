/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::GitHostConnectionRefusal;
use crate::access::has_permission;
use crate::api_spec::ProjectPath;
use crate::authorization::{
    cookie_value, random_url_safe, session_claims, sign_claims, verify_claims,
};
use crate::endpoints::auth::{csrf_clear_cookie, csrf_set_cookie};
use crate::error::{WebError, WebResult, is_unique_violation};
use crate::names::GitHostConnectionName;
use crate::permissions::Permission;
use crate::resource::Resource;
use crate::scope::{ProjectScope, RequiresUnmanaged, can};
use anyhow::Context;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_ci::git_host_connection_token_refresh::access_token_expires_at_from_lifetime_seconds;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::git_host_connections::{self as connections, GitHostConnectionOwnerByDatabaseId};
use gradient_git_host::gitea_oauth::{
    build_git_host_oauth_authorization_url, exchange_authorization_code_for_tokens,
};
use gradient_git_host::provision::{self, ForgeAccess};
use gradient_types::*;
use gradient_util::http::public_client;
use sea_orm::ActiveValue::Set;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;

const GIT_HOST_OAUTH_CONNECT_COOKIE: &str = "git_host_oauth_connect";

#[derive(Serialize, Deserialize)]
struct SignedCookieClaims {
    exp: i64,
    iat: i64,
    state: String,
    pkce_verifier: String,
    user_id: UserId,
    session_id: SessionId,
    project_id: ProjectId,
    project_name: String,
    name: String,
    display_name: String,
}

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GiteaOAuth2ConnectQueryParameters {
    /// Name of a new connection to the Git host.
    #[param(value_type = String)]
    pub name: GitHostConnectionName,
    /// Name to display in the UI. Default is `name`.
    #[serde(default)]
    pub display_name: Option<String>,
}

fn gitea_oauth2_redirect_uri(serve_url: &str) -> String {
    format!(
        "{}/api/v1/git-host-connections/oauth/callback",
        serve_url.trim_end_matches('/')
    )
}

fn build_redirect_response_with_location_and_set_cookie(
    location: &str,
    cookie: String,
) -> WebResult<Response> {
    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", location)
        .header("Set-Cookie", cookie)
        .body(Body::empty())
        .context("build redirect response")
        .map_err(Into::into)
}

async fn connection_name_taken(web_db: &WebDb, project: ProjectId, name: &str) -> WebResult<bool> {
    Ok(connections::by_name(
        web_db,
        GitHostConnectionOwnerByDatabaseId::Project(project),
        name,
    )
    .await?
    .is_some())
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git-host-connections/oauth/connect",
    operation_id = "connectGitHostWithOAuth",
    tag = "git-host-connections",
    summary = "Connect Gitea or Forgejo with OAuth2",
    description = "Redirect a signed-in browser to a Gitea or Forgejo instance (HTTP 302). That instance can run Gradient authorization setup. Use PKCE (S256). Set a short-lived signed `git_host_oauth_connect` cookie with state, project and connection name. The callback can create a project connection named `name` after approval. The connection can receive events and report commit status. Gradient can refresh the access token of that connection automatically. The callback can also link that connection to project tasks that have no connection on the same Git host. The callback can install webhooks in those repositories. API keys can't call this endpoint.",
    params(ProjectPath, GiteaOAuth2ConnectQueryParameters),
    responses((status = FOUND, description = "Redirect to Git host"), WebError)
)]
pub async fn redirect_signed_in_browser_to_git_host_for_project_oauth_approval(
    ProjectScope { project, user, .. }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<GiteaOAuth2ConnectQueryParameters>,
) -> WebResult<Response> {
    let app = state
        .config
        .gitea_app
        .as_ref()
        .ok_or(GitHostConnectionRefusal::GiteaOAuth2ApplicationNotConfigured)?;
    let session = session_claims(&state.jwt_secret, &headers)
        .filter(|claims| claims.id == user.id)
        .ok_or(GitHostConnectionRefusal::GitHostOAuthConnectNeedsSignedInBrowserSession)?;
    let name = query.name.into_inner();
    if connection_name_taken(&state.web_db, project.id, &name).await? {
        return Err(WebError::already_exists(Resource::GitHostConnectionName));
    }
    let display_name = query
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&name)
        .to_owned();

    let csrf_state = random_url_safe(32);
    let pkce_verifier = random_url_safe(32);
    let pkce_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));
    let auth_url = build_git_host_oauth_authorization_url(
        app,
        &gitea_oauth2_redirect_uri(&state.config.server.serve_url),
        &csrf_state,
        &pkce_challenge,
    )?;

    let now = Utc::now();
    let claims = SignedCookieClaims {
        iat: now.timestamp(),
        exp: (now + Duration::minutes(10)).timestamp(),
        state: csrf_state,
        pkce_verifier,
        user_id: user.id,
        session_id: session.jti,
        project_id: project.id,
        project_name: project.name,
        name,
        display_name,
    };
    let cookie = sign_claims(&state.jwt_secret, &claims)
        .context("Cookie Signing for the OAuth2 Connection")?;
    build_redirect_response_with_location_and_set_cookie(
        auth_url.as_str(),
        csrf_set_cookie(
            GIT_HOST_OAUTH_CONNECT_COOKIE,
            &cookie,
            state.config.server.use_tls,
        ),
    )
}

#[utoipa::path(
    get,
    path = "/git-host-connections/oauth/callback",
    operation_id = "gitHostOAuthCallback",
    tag = "git-host-connections",
    summary = "Gitea or Forgejo OAuth2 Callback",
    description = "Redirects from the Git host arrive here. Verify `state` against the `git_host_oauth_connect` cookie first. Exchange the code for a token. Create the connection while the session that started the flow is still valid. Then redirect to the connection page for the project's Git host with `oauth_connected` and `webhooks_installed`, or with the `oauth_error` code.",
    params(("code" = String, Query), ("state" = String, Query)),
    responses((status = FOUND, description = "Redirect to the frontend"), WebError)
)]
pub async fn get_git_host_oauth_callback(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let location = complete_gitea_oauth2_callback_and_return_redirect_url(
        &state.web_db,
        &state.http,
        &state.config,
        &state.jwt_secret,
        &headers,
        &query,
    )
    .await;
    build_redirect_response_with_location_and_set_cookie(
        &location,
        csrf_clear_cookie(GIT_HOST_OAUTH_CONNECT_COOKIE, state.config.server.use_tls),
    )
}

async fn complete_gitea_oauth2_callback_and_return_redirect_url(
    web_db: &WebDb,
    http: &reqwest::Client,
    config: &RuntimeConfig,
    jwt_secret: &SecretString,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> String {
    let claims = cookie_value(headers, GIT_HOST_OAUTH_CONNECT_COOKIE)
        .and_then(|cookie| verify_claims::<SignedCookieClaims>(jwt_secret, cookie).ok())
        .filter(|claims| {
            query
                .get("state")
                .is_some_and(|s| s.as_bytes().ct_eq(claims.state.as_bytes()).into())
        });
    let Some(claims) = claims else {
        return "/?oauth_error=expired".to_owned();
    };
    let page = format!("/project/{}/git-host-connections", claims.project_name);

    let outcome: Result<usize, &'static str> = async {
        let app = config.gitea_app.as_ref().ok_or("failed")?;
        let code = match (query.get("code"), query.get("error")) {
            (_, Some(_)) => return Err("denied"),
            (Some(code), None) => code,
            (None, None) => return Err("failed"),
        };

        let signed_in = gradient_db::lookup::by_id::<ESession, _>(web_db, claims.session_id)
            .await
            .is_ok_and(|session| {
                session.is_some_and(|s| {
                    s.user_id == claims.user_id && s.is_usable_at(gradient_types::now())
                })
            });
        if !signed_in {
            return Err("signed_out");
        }
        let project_unmanaged =
            gradient_db::lookup::by_id::<EProject, _>(web_db, claims.project_id)
                .await
                .is_ok_and(|project| project.is_some_and(|p| !p.managed));
        let allowed = project_unmanaged
            && has_permission(
                web_db,
                claims.user_id,
                claims.project_id,
                Permission::ManageIntegrations,
                None,
            )
            .await
            .unwrap_or(false);
        if !allowed {
            return Err("forbidden");
        }
        if connection_name_taken(web_db, claims.project_id, &claims.name)
            .await
            .unwrap_or(true)
        {
            return Err("exists");
        }

        let failed = |e: &dyn std::fmt::Display| {
            tracing::warn!(error = %e, "Failed to connect to Git host via OAuth2.");
            "failed"
        };
        let tokens = exchange_authorization_code_for_tokens(
            http,
            app,
            code,
            &gitea_oauth2_redirect_uri(&config.server.serve_url),
            &claims.pkce_verifier,
        )
        .await
        .map_err(|e| failed(&format!("{e:#}")))?;

        let crypt_file = config.secrets.crypt_file.as_str();
        let webhook_secret = super::new_webhook_secret();
        let encrypt = |plain: &str| {
            encrypt_secret_with_file(crypt_file, plain).map_err(|e| failed(&format!("{e:#}")))
        };
        let now = gradient_types::now();
        let connection = MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            project: Some(claims.project_id),
            owner_team: None,
            name: claims.name.clone(),
            display_name: claims.display_name.clone(),
            git_host_type: app.git_host_type,
            secret: Some(encrypt(&webhook_secret)?),
            base_url: Some(app.instance_base_url.clone()),
            access_token: Some(encrypt(&tokens.access_token)?),
            encrypted_refresh_token: tokens.refresh_token.as_deref().map(encrypt).transpose()?,
            access_token_expires_at: access_token_expires_at_from_lifetime_seconds(
                now,
                tokens.expires_in,
            ),
            allowed_ips: None,
            receives_events: true,
            reports_status: true,
            github_installation_id: None,
            account_login: None,
            created_by: claims.user_id,
            created_at: now,
            managed: false,
        };
        let created = gradient_db::rows::insert::<EGitHostConnection, _>(web_db, connection)
            .await
            .map_err(|e| {
                if is_unique_violation(&e) {
                    "exists"
                } else {
                    failed(&e)
                }
            })?;

        Ok(
            install_gradient_webhooks_on_project_task_repositories_for_new_gitea_connection(
                web_db,
                &config.server.serve_url,
                &created,
                &webhook_secret,
                &tokens.access_token,
            )
            .await,
        )
    }
    .await;

    match outcome {
        Ok(webhooks_installed) => format!(
            "{page}?oauth_connected={}&webhooks_installed={webhooks_installed}",
            claims.name
        ),
        Err(code) => format!("{page}?oauth_error={code}"),
    }
}

async fn install_gradient_webhooks_on_project_task_repositories_for_new_gitea_connection(
    web_db: &WebDb,
    serve_url: &str,
    connection: &MGitHostConnection,
    webhook_secret: &str,
    access_token: &str,
) -> usize {
    let (Some(project), Some(base_url), Some(webhook_url)) = (
        connection.project,
        connection.base_url.as_deref(),
        super::webhook_url(serve_url, connection),
    ) else {
        return 0;
    };
    let Ok(tasks) = gradient_db::tasks::of_project(project).all(web_db).await else {
        return 0;
    };

    let mut installed = 0;
    for task in tasks.into_iter().filter(|t| {
        !t.managed
            && t.git_host_connection.is_none()
            && super::check_task_repository_url_can_use_git_host_connection(
                &t.repository,
                connection,
            )
            .is_ok()
    }) {
        let Some(repository) = provision::repository_path(&task.repository) else {
            continue;
        };
        let task_id = task.id;
        let mut active: ATask = task.into();
        active.git_host_connection = Set(Some(connection.id));
        if let Err(e) = gradient_db::tasks::update(web_db, active).await {
            tracing::warn!(task = %task_id, error = %e, "Failed to associate the new connection with the task.");
            continue;
        }
        let access = ForgeAccess {
            git_host: connection.git_host_type,
            endpoint_url: base_url,
            token: access_token,
            repository: &repository,
        };
        match provision::install_webhook(
            public_client().clone(),
            &access,
            &webhook_url,
            webhook_secret,
        )
        .await
        {
            Ok(()) => installed += 1,
            Err(e) => {
                tracing::warn!(task = %task_id, error = %e, "Failed to install the task webhook");
            }
        }
    }
    installed
}
