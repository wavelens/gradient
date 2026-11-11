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
    description = "This endpoint can rebuild a set of resources into declarative `services.gradient.state` format.\nThe resources are current users, projects, tasks, cache,\ncustom roles, API keys, teams with members and grants, workers\n(team workers list their own team name) and connections to Git hosts.\nAdmins can copy the running system back into their own nix config.\n\nFields of the credentials file (`password_file`, `private_key_file`,\n`signing_key_file`, `token_file`, `key_file`, `secret_file`,\n`access_token_file`) go into `null` as redacted values. The admin can't restore hashed or encrypted\nsecrets. Enter the secrets by hand. Connections for GitHub App\ngo into the export with `installation_id` /\n`account_login`. Automatically managed `build-request` tasks and\nbuilt-in roles `Admin`/`Write`/`View` stay out of the export.\n\nThe export needs `superuser`.",
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
