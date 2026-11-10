/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_types::BaseResponse;
use gradient_types::cli::EffectiveConfigOption;

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;

#[utoipa::path(
    get,
    path = "/config",
    operation_id = "getServerOptions",
    tag = "admin",
    summary = "Get server options",
    description = "List every valid server option with its group, CLI flag, environment variable, current value, and value source. A value source is one of `flag`, `env`, `default`, `unset`. Secret options such as database URL, Sentry DSN, and S3 access key ID have `secret: true` and no value. Options that NixOS modules set show `env` or `flag`, depending on how the module can pass them. Add `superuser` as a requirement.",
    responses((status = OK, description = "Valid server options", body = Vec<EffectiveConfigOption>), WebError)
)]
pub async fn get_server_options_for_instance_admins(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<Vec<EffectiveConfigOption>>>> {
    Ok(ok_json(state.config.effective_options.clone()))
}
