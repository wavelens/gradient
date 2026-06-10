/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /admin/state` - export the live system as a declarative
//! `services.gradient.state` configuration so operators can codify it in nix.
//!
//! `?format=nix` (default) returns a ready-to-paste Nix expression as
//! `text/plain`; `?format=json` returns the `StateConfiguration` JSON wrapped in
//! the standard response envelope. Secret `*_file` fields are redacted to null.

use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, http::header};
use gradient_state::export;
use gradient_types::{MUser};
use gradient_core::ServerState;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

pub async fn export_state(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(query): Query<ExportQuery>,
) -> WebResult<Response> {
    require_superuser(&user)?;

    let format = query.format.as_deref().unwrap_or("nix");
    if !matches!(format, "nix" | "json") {
        return Err(WebError::bad_request(format!(
            "unknown format '{format}': expected 'nix' or 'json'"
        )));
    }

    let config = export::export_state(&state.web_db)
        .await
        .map_err(|e| WebError::internal(format!("export_state: {e}")))?;
    let redacted = export::redact(&config);

    Ok(match format {
        "json" => ok_json(redacted).into_response(),
        _ => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            export::to_nix(&redacted),
        )
            .into_response(),
    })
}
