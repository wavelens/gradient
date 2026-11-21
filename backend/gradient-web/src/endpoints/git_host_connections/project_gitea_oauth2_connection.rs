/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::GitHostConnectionRefusal;
use crate::api_spec::ProjectPath;
use crate::audit::Audit;
use crate::authorization::{
    cookie_value, random_url_safe, session_claims, sign_claims, verify_claims,
};
use crate::endpoints::auth::{csrf_clear_cookie, csrf_set_cookie};
use crate::error::{WebError, WebResult};
use crate::extractors::{JsonBody, Query};
use crate::helpers::ok_json;
use crate::names::GitHostConnectionName;
use crate::resource::Resource;
use crate::scope::{ProjectScope, RequiresUnmanaged, can};
use anyhow::Context;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_ci::git_host_connection_token_refresh::access_token_expires_at_from_lifetime_seconds;
use gradient_core::ServerState;
use gradient_db::git_host_connections::{self as connections, GitHostConnectionOwnerByDatabaseId};
use gradient_db::{DbContext, WebDb};
use gradient_git_host::gitea_oauth::{
    build_git_host_oauth_authorization_url, exchange_authorization_code_for_tokens,
};
use gradient_git_host::provision::{self, ForgeAccess};
use gradient_types::events::audit::Action;
use gradient_types::*;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;

fn gitea_oauth2_connection_cookie_name(use_tls: bool) -> &'static str {
    if use_tls {
        "__Host-git_host_oauth_connect"
    } else {
        "git_host_oauth_connect"
    }
}

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
    cookie: Option<String>,
) -> WebResult<Response> {
    let mut response = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location);
    if let Some(cookie) = cookie {
        response = response.header(header::SET_COOKIE, cookie);
    }
    response
        .body(Body::empty())
        .context("build redirect response")
        .map_err(Into::into)
}

fn read_verified_oauth2_connect_cookie_claims_matching_state(
    jwt_secret: &SecretString,
    headers: &HeaderMap,
    use_tls: bool,
    state: Option<&str>,
) -> Option<SignedCookieClaims> {
    cookie_value(headers, gitea_oauth2_connection_cookie_name(use_tls))
        .and_then(|cookie| verify_claims::<SignedCookieClaims>(jwt_secret, cookie).ok())
        .filter(|claims| state.is_some_and(|s| s.as_bytes().ct_eq(claims.state.as_bytes()).into()))
}

fn project_name_in_state(state: &str) -> Option<&str> {
    state
        .split_once('.')
        .map(|(_, project)| project)
        .filter(|project| {
            !project.is_empty()
                && project
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
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
    description = "Redirect a signed-in browser to a Gitea or Forgejo instance (HTTP 302). That instance can run Gradient authorization setup. Use PKCE (S256). Set a short-lived signed `git_host_oauth_connect` cookie with state, session, project and connection name. With TLS the cookie name starts with `__Host-`. A project connection with the same `name` returns 409. After approval the Git host redirects to `gitHostOAuthCallback`, which only redirects to the project page. The project page then calls `completeGitHostOAuthConnection`. That call creates the project connection named `name`, links it to project tasks that have no connection on the same Git host and installs webhooks in their repositories. The connection can receive events and report commit status. Gradient can refresh the access token of that connection automatically. API keys can't call this endpoint.",
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

    let csrf_state = format!("{}.{}", random_url_safe(32), project.name);
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
    let use_tls = state.config.server.use_tls;
    build_redirect_response_with_location_and_set_cookie(
        auth_url.as_str(),
        Some(csrf_set_cookie(
            gitea_oauth2_connection_cookie_name(use_tls),
            &cookie,
            use_tls,
        )),
    )
}

#[utoipa::path(
    get,
    path = "/git-host-connections/oauth/callback",
    operation_id = "gitHostOAuthCallback",
    tag = "git-host-connections",
    summary = "Gitea or Forgejo OAuth2 Callback",
    description = "Redirects from the Git host arrive here. Verify `state` against the `git_host_oauth_connect` cookie. Then redirect to the connection page of the project with `code` and `state`, where the signed-in page calls `completeGitHostOAuthConnection`. Keep the cookie for that call. This endpoint stores nothing and sends no request to the Git host. The callback response is a redirect to the project page with `oauth_error=expired` for a missing cookie or a different `state`, when only letters, digits, `-` and `_` occur after the first dot in `state`. Otherwise the callback response is a redirect to `/?oauth_error=expired`. A denied approval redirects to the project page with `oauth_error=denied`. A redirect without `code` goes to the project page with `oauth_error=failed`. These three redirects clear the cookie.",
    params(("code" = String, Query), ("state" = String, Query)),
    responses((status = FOUND, description = "Redirect to the frontend"), WebError)
)]
pub async fn get_git_host_oauth_callback(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let use_tls = state.config.server.use_tls;
    let clear_cookie = || {
        Some(csrf_clear_cookie(
            gitea_oauth2_connection_cookie_name(use_tls),
            use_tls,
        ))
    };
    let Some(claims) = read_verified_oauth2_connect_cookie_claims_matching_state(
        &state.jwt_secret,
        &headers,
        use_tls,
        query.get("state").map(String::as_str),
    ) else {
        let location = match query.get("state").and_then(|s| project_name_in_state(s)) {
            Some(project) => format!("/project/{project}/git-host-connections?oauth_error=expired"),
            None => "/?oauth_error=expired".to_owned(),
        };
        return build_redirect_response_with_location_and_set_cookie(&location, clear_cookie());
    };
    let page = format!("/project/{}/git-host-connections", claims.project_name);
    match (query.get("code"), query.get("error")) {
        (Some(code), None) => {
            let parameters = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("code", code)
                .append_pair("state", &claims.state)
                .finish();
            build_redirect_response_with_location_and_set_cookie(
                &format!("{page}?{parameters}"),
                None,
            )
        }
        (_, Some(_)) => build_redirect_response_with_location_and_set_cookie(
            &format!("{page}?oauth_error=denied"),
            clear_cookie(),
        ),
        (None, None) => build_redirect_response_with_location_and_set_cookie(
            &format!("{page}?oauth_error=failed"),
            clear_cookie(),
        ),
    }
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CompleteGitHostOAuthConnectionRequest {
    /// `code` from the redirect of the Git host.
    pub code: String,
    /// `state` from the redirect of the Git host.
    pub state: String,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct CompletedGitHostOAuthConnection {
    /// Name of the new project connection.
    pub name: String,
    /// Number of project task repositories that received a Gradient webhook.
    pub webhooks_installed: usize,
}

#[utoipa::path(
    post,
    path = "/projects/{project}/git-host-connections/oauth/complete",
    operation_id = "completeGitHostOAuthConnection",
    tag = "git-host-connections",
    summary = "Complete a Gitea or Forgejo OAuth2 connection",
    description = "The project page calls this endpoint with `code` and `state` from the callback redirect. Gradient accepts the call only when the `git_host_oauth_connect` cookie matches `state`, and when the same browser session, the same user and this project started the flow. Then Gradient exchanges the code for a token and creates the project connection. Gradient links that connection to project tasks that have no connection on the same Git host and installs webhooks in their repositories. A project connection with the same name returns 409. Every response after the sign-in, permission and request body checks clears the cookie. A refused call stores nothing and sends no request to the Git host. API keys can't call this endpoint.",
    params(ProjectPath),
    request_body = CompleteGitHostOAuthConnectionRequest,
    responses((status = OK, description = "Connection created", body = CompletedGitHostOAuthConnection), WebError)
)]
pub async fn complete_project_git_host_oauth_connection_and_clear_connect_cookie(
    ProjectScope { project, user, .. }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    audit: Audit,
    JsonBody(body): JsonBody<CompleteGitHostOAuthConnectionRequest>,
) -> Response {
    let use_tls = state.config.server.use_tls;
    let mut response =
        complete_gitea_oauth2_connection_started_by_session_and_store_project_git_host_connection(
            &state.db(),
            &state.jwt_secret,
            &headers,
            &project,
            user.id,
            &audit,
            body,
        )
        .await
        .map(ok_json)
        .into_response();
    if let Ok(clear_cookie) = HeaderValue::from_str(&csrf_clear_cookie(
        gitea_oauth2_connection_cookie_name(use_tls),
        use_tls,
    )) {
        response
            .headers_mut()
            .append(header::SET_COOKIE, clear_cookie);
    }
    response
}

async fn complete_gitea_oauth2_connection_started_by_session_and_store_project_git_host_connection(
    db: &DbContext,
    jwt_secret: &SecretString,
    headers: &HeaderMap,
    project: &MProject,
    user: UserId,
    audit: &Audit,
    body: CompleteGitHostOAuthConnectionRequest,
) -> WebResult<CompletedGitHostOAuthConnection> {
    let config = &db.config;
    let app = config
        .gitea_app
        .as_ref()
        .ok_or(GitHostConnectionRefusal::GiteaOAuth2ApplicationNotConfigured)?;
    let session = session_claims(jwt_secret, headers)
        .filter(|claims| claims.id == user)
        .ok_or(GitHostConnectionRefusal::GitHostOAuthConnectNeedsSignedInBrowserSession)?;
    let claims = read_verified_oauth2_connect_cookie_claims_matching_state(
        jwt_secret,
        headers,
        config.server.use_tls,
        Some(&body.state),
    )
    .filter(|claims| {
        claims.session_id == session.jti
            && claims.user_id == user
            && claims.project_id == project.id
    })
    .ok_or(GitHostConnectionRefusal::GitHostOAuthConnectCookieMissingExpiredOrStartedByOtherSessionUserOrProject)?;
    if connection_name_taken(&db.web_db, project.id, &claims.name).await? {
        return Err(WebError::already_exists(Resource::GitHostConnectionName));
    }

    let http = &db.user_supplied_url_http_client;
    let tokens = exchange_authorization_code_for_tokens(
        http,
        app,
        &body.code,
        &gitea_oauth2_redirect_uri(&config.server.serve_url),
        &claims.pkce_verifier,
    )
    .await
    .map_err(|e| {
        tracing::warn!(
            error = format!("{e:#}"),
            "The Git host rejected the authorization code from OAuth2."
        );
        GitHostConnectionRefusal::GitHostRefusedOAuthAuthorizationCode
    })?;

    let crypt_file = config.secrets.crypt_file.as_str();
    let webhook_secret = gradient_ci::actions::new_webhook_secret();
    let encrypt = |plain: &str| {
        encrypt_secret_with_file(crypt_file, plain)
            .context("encrypt the secrets for the OAuth2 connection")
    };
    let now = gradient_types::now();
    let connection = MGitHostConnection {
        id: GitHostConnectionId::now_v7(),
        project: Some(project.id),
        owner_team: None,
        name: claims.name,
        display_name: claims.display_name,
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
        created_by: user,
        created_at: now,
        managed: false,
        refused_signature_count: 0,
        last_refused_signature_at: None,
    };
    let (created, linked_tasks) =
        super::insert_and_link_fitting_tasks(db.web_db.inner(), connection)
            .await
            .map_err(|e| WebError::from_db_err(e, Resource::GitHostConnectionName))?;
    super::write_audit(audit, Action::GitHostConnectionCreate, &created).await;

    let webhooks_installed =
        install_gradient_webhooks_on_project_task_repositories_for_new_gitea_connection(
            http,
            &config.server.serve_url,
            &created,
            linked_tasks,
            &webhook_secret,
            &tokens.access_token,
        )
        .await;
    Ok(CompletedGitHostOAuthConnection {
        name: created.name,
        webhooks_installed,
    })
}

async fn install_gradient_webhooks_on_project_task_repositories_for_new_gitea_connection(
    http: &HttpClientForUserSuppliedUrls,
    serve_url: &str,
    connection: &MGitHostConnection,
    tasks: Vec<MTask>,
    webhook_secret: &str,
    access_token: &str,
) -> usize {
    let (Some(base_url), Some(webhook_url)) = (
        connection.base_url.as_deref(),
        super::webhook_url(serve_url, connection),
    ) else {
        return 0;
    };

    let mut installed = 0;
    for task in tasks {
        let Some(repository) = provision::repository_path(&task.repository) else {
            continue;
        };
        let task_id = task.id;
        let access = ForgeAccess {
            git_host: connection.git_host_type,
            endpoint_url: base_url,
            token: access_token,
            repository: &repository,
        };
        match provision::install_webhook(http.clone(), &access, &webhook_url, webhook_secret).await
        {
            Ok(()) => installed += 1,
            Err(e) => {
                tracing::warn!(task = %task_id, error = %e, "Failed to install the task webhook");
            }
        }
    }
    installed
}
