/*
* SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

pub mod admin;
pub mod auth;
pub mod badges;
pub mod board;
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
pub mod git_host_hooks;
pub mod gradient_ci_connections;
pub mod invites;
pub mod live;
pub mod log_stream;
pub mod metrics;
pub mod metrics_query;
pub mod projects;
pub mod search;
pub mod stars;
pub mod stats;
pub mod tasks;
pub mod teams;
pub mod user;
pub mod user_ssh_keys;
pub mod webhooks;
pub mod worker_tokens;
pub mod workers;

use crate::error::WebResult;
use axum::extract::{Json, State};
use gradient_core::ServerState;
use gradient_types::{BaseResponse, CreatePermission};
use serde::Serialize;
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
        format!("attachment; filename=\"{filename}\"")
    };
    hardened(content_type_for_filename(filename), disposition)
}

pub fn archive_headers(archive_name: &str) -> [(axum::http::HeaderName, String); 4] {
    hardened(
        "application/zstd",
        format!("attachment; filename=\"{archive_name}\""),
    )
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
    crate::error::WebError::not_found_msg("Not Found".to_string())
}

pub async fn get_health() -> WebResult<Json<BaseResponse<String>>> {
    let res = BaseResponse {
        error: false,
        message: "200 ALIVE".to_string(),
    };

    Ok(Json(res))
}

#[derive(Serialize)]
pub struct ServerConfig {
    pub version: String,
    pub oidc_enabled: bool,
    pub oidc_required: bool,
    pub registration_enabled: bool,
    pub email_verification_enabled: bool,
    pub smtp_enabled: bool,
    pub quic: bool,
    pub create_project: CreatePermission,
    pub create_team: CreatePermission,
    pub create_cache: CreatePermission,
    pub github_app_enabled: bool,
    pub ssh_enabled: bool,
    pub ssh_port: Option<u16>,
    pub gradient_ci_enabled: bool,
    pub gradient_ci_url: String,
    pub logo_url: Option<String>,
}

pub async fn get_config(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<ServerConfig>>> {
    let res = BaseResponse {
        error: false,
        message: ServerConfig {
            version: env!("CARGO_PKG_VERSION").to_string(),
            oidc_enabled: state.config.oidc.is_some(),
            oidc_required: state.config.oidc.as_ref().is_some_and(|o| o.required),
            registration_enabled: state.config.registration.enable
                && !state.config.oidc.as_ref().is_some_and(|o| o.required),
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
            ssh_enabled: state.config.ssh.enable,
            ssh_port: state.config.ssh.enable.then_some(state.config.ssh.port),
            gradient_ci_enabled: state.config.gradient_ci.enable,
            gradient_ci_url: state.config.gradient_ci.url.clone(),
            logo_url: state.config.server.frontend_logo_url.clone(),
        },
    };

    Ok(Json(res))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_content_is_sandboxed_without_returning_its_origin() {
        assert!(UNTRUSTED_CONTENT_CSP.starts_with("sandbox"));
        assert!(
            !UNTRUSTED_CONTENT_CSP.contains("allow-same-origin"),
            "allow-same-origin defeats the sandbox: {UNTRUSTED_CONTENT_CSP}"
        );
        assert!(UNTRUSTED_CONTENT_CSP.contains("allow-scripts"));
        assert!(UNTRUSTED_CONTENT_CSP.contains("allow-top-navigation-by-user-activation"));
        assert!(
            !UNTRUSTED_CONTENT_CSP
                .split_whitespace()
                .any(|t| t == "allow-top-navigation"),
            "unconditional top navigation lets the page redirect the viewer: {UNTRUSTED_CONTENT_CSP}"
        );

        let names: Vec<_> = untrusted_content_headers()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(names.contains(&axum::http::header::CONTENT_SECURITY_POLICY));
        assert!(names.contains(&axum::http::header::X_CONTENT_TYPE_OPTIONS));
    }

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
        assert_eq!(
            by_name(axum::http::header::CONTENT_SECURITY_POLICY),
            UNTRUSTED_CONTENT_CSP
        );
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
