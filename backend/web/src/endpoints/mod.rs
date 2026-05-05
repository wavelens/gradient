/*
* SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

pub mod admin;
pub mod auth;
pub mod badges;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod evals;
pub mod forge_hooks;
pub mod orgs;
pub mod projects;
pub mod stats;
pub mod user;
pub mod webhooks;
pub mod workers;

use crate::error::WebResult;
use axum::extract::{Json, State};
use gradient_core::types::{BaseResponse, ServerState};
use serde::Serialize;
use std::sync::Arc;

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
    /// Whether the server advertises HTTP/3 (QUIC) support.
    /// Clients may attempt an HTTP/3 upgrade when this is true.
    /// Actual HTTP/3 termination is handled by the reverse proxy (nginx).
    pub quic: bool,
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
            quic: state.config.proto.quic,
        },
    };

    Ok(Json(res))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::hydra::parse_hydra_product_line;

    #[test]
    fn parse_hydra_product_line_typical() {
        let got = parse_hydra_product_line("file doc /nix/store/xyz/share/doc/index.html");
        assert_eq!(
            got,
            Some((
                "doc".to_string(),
                "/nix/store/xyz/share/doc/index.html".to_string()
            ))
        );
    }

    #[test]
    fn parse_hydra_product_line_rejoins_paths_with_spaces() {
        let got = parse_hydra_product_line("file report /tmp/my report.txt");
        assert_eq!(
            got,
            Some(("report".to_string(), "/tmp/my report.txt".to_string()))
        );
    }

    #[test]
    fn parse_hydra_product_line_rejects_non_file_prefix() {
        assert_eq!(parse_hydra_product_line("dir doc /x"), None);
    }

    #[test]
    fn parse_hydra_product_line_rejects_too_few_parts() {
        assert_eq!(parse_hydra_product_line("file doc"), None);
        assert_eq!(parse_hydra_product_line("file"), None);
        assert_eq!(parse_hydra_product_line(""), None);
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
