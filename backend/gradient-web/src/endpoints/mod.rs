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
pub mod build_requests;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod evals;
pub mod forge_hooks;
pub mod invites;
pub mod live;
pub mod metrics;
pub mod metrics_query;
pub mod projects;
pub mod stats;
pub mod tasks;
pub mod user;
pub mod workers;

use crate::error::WebResult;
use axum::extract::{Json, State};
use gradient_core::ServerState;
use gradient_types::{BaseResponse, CreatePermission};
use serde::Serialize;
use std::sync::Arc;

/// Sandbox applied to every response whose bytes a build controls.
///
/// A build writes its own `nix-support/hydra-build-products`, and any path in a
/// cache can hold an `.html`, so both the filename and the declared subtype are
/// attacker-controlled: any build can ask to be served as `text/html`, inline,
/// from the origin that also serves the API and holds the session cookie.
/// Script in such a page cannot read the `HttpOnly` cookie - but it would not
/// need to. A same-origin `fetch` carries the cookie automatically, `SameSite`
/// does not apply to a request the origin makes of itself, and the reply is
/// readable, so the page could act as the viewer (including minting an API key
/// that outlives their session).
///
/// `sandbox` drops the response into a unique opaque origin, so its script
/// reaches the API as nobody at all. `allow-scripts` keeps interactive reports
/// (coverage, benchmarks) working, and `allow-top-navigation-by-user-activation`
/// lets a multi-page report be clicked through - navigation was never what the
/// attack needed, and requiring a real click still denies a silent redirect to
/// somewhere else. `allow-same-origin` must never join them: that pair hands the
/// origin back and undoes all of this.
///
/// A browser that does not know a token ignores it and keeps the rest, so an old
/// one loses the click-through rather than the isolation.
pub const UNTRUSTED_CONTENT_CSP: &str =
    "sandbox allow-scripts allow-top-navigation-by-user-activation";

/// [`UNTRUSTED_CONTENT_CSP`] plus `nosniff`, for any build-controlled body.
pub fn untrusted_content_headers() -> [(axum::http::HeaderName, &'static str); 2] {
    [
        (
            axum::http::header::CONTENT_SECURITY_POLICY,
            UNTRUSTED_CONTENT_CSP,
        ),
        (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ]
}

/// Full header set for one served build product. `subtype` comes from the
/// build's own `hydra-build-products`, so `html` renders inline - safely, behind
/// [`UNTRUSTED_CONTENT_CSP`] - and everything else downloads.
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

/// Header set for a product directory served as a `.tar.zst` archive.
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
    pub create_cache: CreatePermission,
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
            registration_enabled: state.config.registration.enable_registration
                && !state.config.oidc.as_ref().is_some_and(|o| o.required),
            email_verification_enabled: state.config.email.is_some()
                && state
                    .config
                    .email
                    .as_ref()
                    .is_some_and(|e| e.require_verification),
            smtp_enabled: state.email.is_enabled(),
            quic: state.config.proto.quic,
            create_project: state.config.server.create_project,
            create_cache: state.config.server.create_cache,
        },
    };

    Ok(Json(res))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_util::hydra::parse_hydra_product_line;

    #[test]
    fn parse_hydra_product_line_typical() {
        let got = parse_hydra_product_line("file doc /nix/store/xyz/share/doc/index.html");
        assert_eq!(
            got,
            Some((
                "file".to_string(),
                "doc".to_string(),
                "/nix/store/xyz/share/doc/index.html".to_string()
            ))
        );
    }

    #[test]
    fn parse_hydra_product_line_accepts_any_type() {
        assert_eq!(
            parse_hydra_product_line("doc readme /nix/store/xyz/README.md"),
            Some((
                "doc".to_string(),
                "readme".to_string(),
                "/nix/store/xyz/README.md".to_string()
            ))
        );
    }

    #[test]
    fn parse_hydra_product_line_rejects_too_few_parts() {
        assert_eq!(parse_hydra_product_line("file doc"), None);
        assert_eq!(parse_hydra_product_line("file"), None);
        assert_eq!(parse_hydra_product_line(""), None);
    }

    /// The sandbox is the whole defence: it puts a build-authored page in an
    /// opaque origin so its script cannot act as the viewer against our API.
    /// `allow-same-origin` alongside `allow-scripts` would hand the origin back.
    #[test]
    fn untrusted_content_is_sandboxed_without_returning_its_origin() {
        assert!(UNTRUSTED_CONTENT_CSP.starts_with("sandbox"));
        assert!(
            !UNTRUSTED_CONTENT_CSP.contains("allow-same-origin"),
            "allow-same-origin defeats the sandbox: {UNTRUSTED_CONTENT_CSP}"
        );
        // Interactive reports (coverage, benchmarks) must keep working, and a
        // multi-page one must stay clickable - but only on a real activation,
        // so the page still cannot redirect the viewer on its own.
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

    /// An HTML product still renders inline - that is the feature - but never
    /// without the sandbox that makes rendering it safe.
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
    fn content_type_for_known_extensions() {
        assert_eq!(content_type_for_filename("x.html"), "text/html");
        assert_eq!(content_type_for_filename("x.htm"), "text/html");
        assert_eq!(content_type_for_filename("x.tar"), "application/x-tar");
        assert_eq!(content_type_for_filename("x.gz"), "application/gzip");
        assert_eq!(content_type_for_filename("x.zst"), "application/zstd");
        assert_eq!(content_type_for_filename("x.txt"), "text/plain");
        assert_eq!(content_type_for_filename("x.json"), "application/json");
        assert_eq!(content_type_for_filename("x.zip"), "application/zip");
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
