/*
* SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

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

use crate::error::{WebError, WebResult};
use axum::extract::{Json, State};
use core::db::get_any_organization_by_name;
use core::types::{BaseResponse, MOrganization, MUser, ServerState};
use core::types::{COrganizationUser, EOrganizationUser};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

/// Load an organization by name and verify the caller may read it.
///
/// Returns `not_found(label)` when the org doesn't exist *or* when it is
/// private and `maybe_user` is not a member — the caller cannot distinguish
/// the two cases, which prevents org-existence enumeration.
///
/// `label` is the resource name in the error message; project endpoints pass
/// `"Project"` so the response doesn't leak org existence.
pub async fn get_org_readable(
    state: &Arc<ServerState>,
    org_name: String,
    maybe_user: &Option<MUser>,
    label: &str,
) -> WebResult<MOrganization> {
    let org = get_any_organization_by_name(Arc::clone(state), org_name)
        .await?
        .ok_or_else(|| WebError::not_found(label))?;

    if !org.public {
        let is_member = match maybe_user {
            Some(user) => user_is_org_member(state, user.id, org.id).await?,
            None => false,
        };
        if !is_member {
            return Err(WebError::not_found(label));
        }
    }

    Ok(org)
}

pub async fn user_is_org_member(
    state: &Arc<ServerState>,
    user_id: Uuid,
    organization_id: Uuid,
) -> Result<bool, WebError> {
    Ok(EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.db)
        .await?
        .is_some())
}

// ── Hydra build product helpers ───────────────────────────────────────────────

/// Parse a single `hydra-build-products` line.
///
/// Returns `(file_type, file_path)` for lines of the form `file <type> <path>`,
/// `None` for blank lines or lines with a different prefix.
pub fn parse_hydra_product_line(line: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 3 && parts[0] == "file" {
        Some((parts[1].to_string(), parts[2..].join(" ")))
    } else {
        None
    }
}

pub fn content_type_for_filename(filename: &str) -> &'static str {
    match std::path::Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        Some("tar") => "application/x-tar",
        Some("gz") => "application/gzip",
        Some("zst") => "application/zstd",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

pub async fn handle_404() -> WebError {
    WebError::NotFound("Not Found".to_string())
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
            oidc_enabled: state.cli.oidc_enabled,
            oidc_required: state.cli.oidc_required,
            registration_enabled: state.cli.enable_registration && !state.cli.oidc_required,
            email_verification_enabled: state.cli.email_enabled
                && state.cli.email_require_verification,
            quic: state.cli.quic,
        },
    };

    Ok(Json(res))
}

#[cfg(test)]
mod tests {
    use super::*;

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
