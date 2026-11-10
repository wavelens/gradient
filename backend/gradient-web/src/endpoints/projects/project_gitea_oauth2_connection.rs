/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::integrations::{
    IntegrationResponse, IntegrationToCreate, SharedIntegrationCreationFields, WebhookBase,
    create_integrations_and_attach_matching_tasks,
};
use super::refusal::ProjectRefusal;
use crate::access::has_permission;
use crate::api_spec::ProjectPath;
use crate::authorization::{
    cookie_value, random_url_safe, session_claims, sign_claims, verify_claims,
};
use crate::endpoints::auth::{csrf_clear_cookie, csrf_set_cookie};
use crate::endpoints::tasks::auto_attach::url_host;
use crate::error::{WebError, WebResult};
use crate::names::IntegrationName;
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
use gradient_ci::IntegrationKind;
use gradient_ci::outbound_integration_access_token::access_token_expires_at_from_lifetime_seconds;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_git_host::gitea_oauth::{
    build_git_host_oauth_authorization_url, exchange_authorization_code_for_tokens,
};
use gradient_git_host::provision::{self, ForgeAccess};
use gradient_types::*;
use gradient_util::http::public_client;
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
    /// Combined name of the webhook receiver and status reporter.
    #[param(value_type = String)]
    pub name: IntegrationName,
    /// Name to display in the UI. Default is `name`.
    #[serde(default)]
    pub display_name: Option<String>,
}

fn gitea_oauth2_redirect_uri(serve_url: &str) -> String {
    format!(
        "{}/api/v1/integrations/oauth/callback",
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

async fn integration_name_taken(web_db: &WebDb, project: ProjectId, name: &str) -> WebResult<bool> {
    for kind in [IntegrationKind::Inbound, IntegrationKind::Outbound] {
        if gradient_db::integrations::by_name(web_db, project, kind, name)
            .await?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[utoipa::path(
    get,
    path = "/projects/{project}/integrations/oauth/connect",
    operation_id = "connectGitHostWithOAuth",
    tag = "integrations",
    summary = "Connect Gitea or Forgejo with OAuth2",
    description = "Redirect the signed-in browser to a Gitea or Forgejo instance with a setup to authorize Gradient (HTTP 302). Use PKCE (S256). Set a short-lived signed `git_host_oauth_connect` cookie with the state, project and integration name. The callback can create a webhook receiver named `name` and a status reporter after approval. Gradient can refresh the access token for the status reporter automatically. The callback can also install a webhook on the repositories of tasks that connect to the new integration. API keys cannot call this endpoint.",
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
        .ok_or(ProjectRefusal::GiteaOAuth2ApplicationNotConfigured)?;
    let session = session_claims(&state.jwt_secret, &headers)
        .filter(|claims| claims.id == user.id)
        .ok_or(ProjectRefusal::GitHostOAuthConnectNeedsSignedInBrowserSession)?;
    let name = query.name.into_inner();
    if integration_name_taken(&state.web_db, project.id, &name).await? {
        return Err(WebError::already_exists(Resource::IntegrationName));
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
    path = "/integrations/oauth/callback",
    operation_id = "gitHostOAuthCallback",
    tag = "integrations",
    summary = "Gitea or Forgejo OAuth2 Callback",
    description = "Handle the redirect from the Git host. Start with validation of `state` against the `git_host_oauth_connect` cookie. Exchange the code for a token. Create the integration while the session that started the connection is still active. Redirect to the project integration page with `oauth_connected` and `webhooks_installed`, or with the `oauth_error` code.",
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
    let page = format!("/project/{}/integrations", claims.project_name);

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
        if integration_name_taken(web_db, claims.project_id, &claims.name)
            .await
            .unwrap_or(true)
        {
            return Err("exists");
        }

        let failed = |e: &dyn std::fmt::Display| {
            tracing::warn!(error = %e, "Failed to connect to Git host over OAuth2");
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

        let created = create_integrations_and_attach_matching_tasks(
            web_db,
            &config.secrets.crypt_file,
            &WebhookBase {
                serve_url: &config.server.serve_url,
                project: &claims.project_name,
            },
            SharedIntegrationCreationFields {
                project: claims.project_id,
                name: claims.name.clone(),
                display_name: claims.display_name.clone(),
                git_host: app.git_host_type,
                endpoint_url: Some(app.instance_base_url.clone()),
                created_by: claims.user_id,
            },
            vec![
                IntegrationToCreate {
                    kind: IntegrationKind::Inbound,
                    secret: None,
                    access_token: None,
                    refresh_token: None,
                    access_token_expires_at: None,
                    allowed_ips: None,
                },
                IntegrationToCreate {
                    kind: IntegrationKind::Outbound,
                    secret: None,
                    access_token: Some(&tokens.access_token),
                    refresh_token: tokens.refresh_token.as_deref(),
                    access_token_expires_at: access_token_expires_at_from_lifetime_seconds(
                        gradient_types::now(),
                        tokens.expires_in,
                    ),
                    allowed_ips: None,
                },
            ],
        )
        .await
        .map_err(|e| failed(&e))?;
        let inbound = created.first().ok_or("failed")?;

        Ok(
            install_gradient_webhook_on_repositories_of_tasks_attached_to_new_integration(
                web_db,
                app,
                claims.project_id,
                inbound,
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

async fn install_gradient_webhook_on_repositories_of_tasks_attached_to_new_integration(
    web_db: &WebDb,
    app: &GiteaOAuthConfig,
    project: ProjectId,
    inbound: &IntegrationResponse,
    access_token: &str,
) -> usize {
    let (Some(secret), Some(webhook_url)) = (
        inbound.generated_secret.as_deref(),
        inbound.webhook_url.as_deref(),
    ) else {
        return 0;
    };
    let Ok(tasks) = gradient_db::tasks::of_project(project).all(web_db).await else {
        return 0;
    };
    let app_host = url_host(&app.instance_base_url);

    let mut installed = 0;
    for task in tasks.iter().filter(|t| {
        inbound.connected_tasks.contains(&t.name) && url_host(&t.repository) == app_host
    }) {
        let Some(repository) = provision::repository_path(&task.repository) else {
            continue;
        };
        let access = ForgeAccess {
            git_host: app.git_host_type,
            endpoint_url: &app.instance_base_url,
            token: access_token,
            repository: &repository,
        };
        match provision::install_webhook(public_client().clone(), &access, webhook_url, secret)
            .await
        {
            Ok(()) => installed += 1,
            Err(e) => {
                tracing::warn!(task = %task.id, error = %e, "Failed to install the task webhook");
            }
        }
    }
    installed
}
