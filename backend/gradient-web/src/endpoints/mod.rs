/*
* SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

pub(crate) mod accept_header_parsing;
pub mod access;
pub mod admin;
pub mod auth;
pub mod badges;
pub mod board;
pub mod board_abort;
pub mod board_cache;
pub mod board_clusters;
pub mod board_metrics;
pub mod board_storage;
pub mod board_subjects;
pub mod build_requests;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod dashboard;
pub mod evals;
pub mod events;
pub mod exec_jobs;
pub mod git_host_connections;
pub mod git_host_hooks;
pub mod gradient_ci_connections;
pub mod invites;
pub mod live;
pub mod log_stream;
pub mod login_providers;
pub(crate) mod membership;
pub mod metrics;
pub mod metrics_query;
pub mod projects;
mod refusal;
pub mod search;
pub mod stars;
pub mod stats;
pub mod tasks;
pub mod teams;
pub mod user;
pub mod user_ssh_keys;
pub mod webhooks;
pub mod worker_setup;
pub mod worker_tokens;
pub mod yearly_instance_summary;

use crate::error::{WebError, WebResult};
use anyhow::Context;
use axum::extract::{Json, State};
pub use gradient_api_types::{ChunkReceived, GiteaOAuthAppConfig, LoginProviderInfo, ServerConfig};
use gradient_core::ServerState;
use gradient_git_host::github_app::GitHubAppPageUrlCache;
use gradient_types::BaseResponse;
use gradient_types::config::GitHubAppConfig;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::shutdown::Shutdown;
use std::sync::Arc;

/// `sandbox` is dropping a build-controlled response into a unique opaque origin. Its script is
/// reaching the API as nobody. A same-origin `fetch` would otherwise carry the session cookie and
/// act as the viewer. `allow-same-origin` must never join `allow-scripts`, as that pair is handing
/// the origin back.
pub const UNTRUSTED_CONTENT_CSP: &str =
    "sandbox allow-scripts allow-top-navigation-by-user-activation";

pub fn untrusted_content_headers() -> [(axum::http::HeaderName, &'static str); 2] {
    [
        (
            axum::http::header::CONTENT_SECURITY_POLICY,
            UNTRUSTED_CONTENT_CSP,
        ),
        (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ]
}

pub fn build_product_headers(
    filename: &str,
    subtype: &str,
) -> [(axum::http::HeaderName, String); 4] {
    let disposition = if subtype == "html" {
        "inline".to_string()
    } else {
        format!("attachment; filename=\"{}\"", attachment_name(filename))
    };
    hardened(content_type_for_filename(filename), disposition)
}

pub fn archive_headers(archive_name: &str) -> [(axum::http::HeaderName, String); 4] {
    hardened(
        "application/zstd",
        format!("attachment; filename=\"{}\"", attachment_name(archive_name)),
    )
}

pub(crate) fn attachment_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn hardened(content_type: &str, disposition: String) -> [(axum::http::HeaderName, String); 4] {
    let [csp, nosniff] = untrusted_content_headers();
    [
        (axum::http::header::CONTENT_TYPE, content_type.to_string()),
        (axum::http::header::CONTENT_DISPOSITION, disposition),
        (csp.0, csp.1.to_string()),
        (nosniff.0, nosniff.1.to_string()),
    ]
}

pub fn content_type_for_filename(filename: &str) -> &'static str {
    match std::path::Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        Some("html") | Some("htm") => "text/html",
        Some("tar") => "application/x-tar",
        Some("gz") => "application/gzip",
        Some("zst") => "application/zstd",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

pub async fn handle_404() -> crate::error::WebError {
    crate::error::WebError::from(refusal::EndpointRefusal::UnknownRoute)
}

#[utoipa::path(
    get,
    path = "/health",
    operation_id = "getHealth",
    tag = "general",
    summary = "Health check",
    description = "Returns `200 ALIVE` when the server is running. Does not require authentication.",
    responses((status = OK, description = "Server is alive", body = String, content_type = "application/json"), WebError)
)]
pub async fn get_health() -> WebResult<Json<BaseResponse<String>>> {
    let res = BaseResponse {
        error: false,
        message: "200 ALIVE".to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    get,
    path = "/config",
    operation_id = "getConfig",
    tag = "general",
    summary = "Server configuration",
    description = "Return the frontend feature flags (OIDC and login providers, password login, registration, email confirmation). `serve_url` is the public base URL of this server. Snippets such as the Nix substituter URL for the cache contain that URL.",
    responses((status = OK, description = "Server configuration", body = ServerConfig), WebError)
)]
pub async fn get_config(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<ServerConfig>>> {
    let github_app_page_url = github_app_page_url_from_cache_or_refresh_in_background(
        state.config.github_app.as_ref(),
        &state.user_supplied_url_http_client,
        &state.memory.github_app_page_url,
        &state.shutdown,
    );
    let res = BaseResponse {
        error: false,
        message: ServerConfig {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: state.config.server.source_commit_hash.clone(),
            oidc_enabled: state.config.oidc.is_some(),
            login_providers: state
                .extensions
                .login_providers
                .iter()
                .map(|p| LoginProviderInfo {
                    id: p.id().to_owned(),
                    label: p.label().to_owned(),
                })
                .collect(),
            password_login_enabled: state.login_methods().password_login(),
            registration_enabled: state.config.registration.enable
                && state.login_methods().password_login(),
            email_verification_enabled: state.config.email.is_some()
                && state
                    .config
                    .email
                    .as_ref()
                    .is_some_and(|e| e.require_verification),
            smtp_enabled: state.email.is_enabled(),
            quic: state.config.server.use_quic,
            create_project: state.config.permissions.create_project,
            create_team: state.config.permissions.create_team,
            create_cache: state.config.permissions.create_cache,
            github_app_enabled: state.config.github_app.is_some(),
            github_app_install_url: github_app_page_url
                .as_deref()
                .map(gradient_git_host::github_app::install_url),
            github_app_page_url,
            gitea_oauth_app: state
                .config
                .gitea_app
                .as_ref()
                .map(|app| GiteaOAuthAppConfig {
                    git_host_type: app.git_host_type.as_path_segment().to_owned(),
                    instance_base_url: app.instance_base_url.clone(),
                }),
            ssh_enabled: state.config.ssh.enable,
            ssh_port: state.config.ssh.enable.then_some(state.config.ssh.port),
            gradient_ci_enabled: state.config.gradient_ci.enable,
            gradient_ci_url: gradient_types::consts::GRADIENT_CI_URL.to_owned(),
            serve_url: state.config.server.serve_url.clone(),
            logo_url: state.config.server.frontend_logo_url.clone(),
            exec_enabled: state.config.exec.enable,
            public_listing: state.config.server.public_listing,
            max_keep_evaluations_per_task: state.config.eval.keep_evaluations_max(),
            max_cached_nar_storage_gb: Some(state.config.cache.max_storage_gb).filter(|gb| *gb > 0),
        },
    };

    Ok(Json(res))
}

const GITHUB_APP_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) fn github_app_page_url_from_cache_or_refresh_in_background(
    github_app: Option<&GitHubAppConfig>,
    http: &HttpClientForUserSuppliedUrls,
    github_app_page_url: &Arc<GitHubAppPageUrlCache>,
    shutdown: &Shutdown,
) -> Option<String> {
    let app = github_app?.clone();
    let http = http.clone();
    github_app_page_url.get_cached_page_url_and_start_background_lookup_when_stale_or_missing(
        shutdown,
        || async move {
            let pem = tokio::fs::read_to_string(&app.private_key_file)
                .await
                .context("reading the GitHub App private key")?;
            tokio::time::timeout(
                GITHUB_APP_LOOKUP_TIMEOUT,
                gradient_git_host::github_app::get_app_page_url(&http, app.app_id, &pem),
            )
            .await
            .context("Timed out during the lookup of the GitHub App")?
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_product_renders_inline_but_always_sandboxed() {
        let headers = build_product_headers("report.html", "html");
        let by_name = |n: axum::http::HeaderName| {
            headers
                .iter()
                .find(|(h, _)| *h == n)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(by_name(axum::http::header::CONTENT_TYPE), "text/html");
        assert_eq!(by_name(axum::http::header::CONTENT_DISPOSITION), "inline");
        let policy = by_name(axum::http::header::CONTENT_SECURITY_POLICY);
        let sandbox: Vec<_> = policy
            .split(';')
            .map(str::trim)
            .find_map(|directive| directive.strip_prefix("sandbox"))
            .expect("served policy is sandboxed")
            .split_whitespace()
            .collect();
        assert!(sandbox.contains(&"allow-scripts"), "{policy}");
        assert!(
            sandbox.contains(&"allow-top-navigation-by-user-activation"),
            "{policy}"
        );
        assert!(!sandbox.contains(&"allow-same-origin"), "{policy}");
        assert!(!sandbox.contains(&"allow-top-navigation"), "{policy}");
        assert_eq!(
            by_name(axum::http::header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
    }

    #[test]
    fn non_html_product_downloads_and_archives_are_hardened_too() {
        let file = build_product_headers("out.bin", "file");
        assert!(
            file.iter()
                .any(|(h, v)| *h == axum::http::header::CONTENT_DISPOSITION
                    && v.starts_with("attachment")),
            "a non-html product must download, not render"
        );
        for headers in [file, archive_headers("out.tar.zst")] {
            assert!(
                headers
                    .iter()
                    .any(|(h, v)| *h == axum::http::header::CONTENT_SECURITY_POLICY
                        && v == UNTRUSTED_CONTENT_CSP)
            );
        }
    }

    #[test]
    fn a_quote_or_newline_in_a_build_filename_cannot_break_the_header() {
        let disposition = |headers: [(axum::http::HeaderName, String); 4]| {
            headers
                .into_iter()
                .find(|(h, _)| *h == axum::http::header::CONTENT_DISPOSITION)
                .map(|(_, v)| v)
        };

        assert_eq!(
            disposition(build_product_headers("a\"b\r\nX: y.bin", "file")).as_deref(),
            Some("attachment; filename=\"a_b__X: y.bin\"")
        );
        assert_eq!(
            disposition(archive_headers("x\\\".tar.zst")).as_deref(),
            Some("attachment; filename=\"x__.tar.zst\"")
        );
    }

    #[test]
    fn content_type_falls_back_to_octet_stream() {
        assert_eq!(
            content_type_for_filename("unknown.xyz"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for_filename("noext"),
            "application/octet-stream"
        );
    }
}
