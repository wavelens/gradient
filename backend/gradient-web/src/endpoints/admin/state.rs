/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::AdminRefusal;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_state::export;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

#[utoipa::path(
    get,
    path = "/state",
    operation_id = "exportState",
    tag = "admin",
    summary = "Export the live state configuration",
    description = "Reconstructs the current users, projects, tasks, caches,\ncustom roles, API keys, teams with their members and grants, workers\n(team workers name their team) and integrations into a\ndeclarative `services.gradient.state` shape, so operators can codify\nthe running system back into their nix configuration.\n\nEvery credential-file field (`password_file`, `private_key_file`,\n`signing_key_file`, `token_file`, `key_file`, `secret_file`,\n`access_token_file`) is redacted to `null` - the hashed/encrypted\nsecrets cannot be recovered and must be filled in by hand. GitHub\nintegration rows are exported with their `installation_id` /\n`account_login`. The auto-managed `build-request` task and the\nbuilt-in `Admin`/`Write`/`View` roles are omitted.\n\nRequires `superuser`.",
    params(ExportQuery),
    responses((status = OK, description = "Exported state configuration", content((String = "text/plain"), (serde_json::Value = "application/json"))), WebError)
)]
pub async fn export_state(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ExportQuery>,
) -> WebResult<Response> {
    let format = query.format.as_deref().unwrap_or("nix");
    if !matches!(format, "nix" | "json") {
        return Err(AdminRefusal::UnknownFormat {
            format: format.to_string(),
        }
        .into());
    }

    let config = export::export_state(&state.web_db)
        .await
        .context("export_state")?;
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
