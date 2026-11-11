/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CreateGitHostConnectionRequest, GitHostConnectionResponse, GitHostConnectionSummary,
    IncomingWebhookDeliveryResponse, PatchGitHostConnectionRequest,
};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::git_host_connections::GitHostConnectionOwnerByDatabaseId as Owner;
use gradient_types::*;
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/git-host-connections",
    operation_id = "listInstanceGitHostConnections",
    tag = "admin",
    summary = "List instance git host connections",
    description = "List the Git host connections of the instance. Each installation of a GitHub App has one. Every project can use them.",
    responses((status = OK, description = "Instance Connection", body = Vec<GitHostConnectionSummary>), WebError)
)]
pub async fn list_instance_git_host_connections(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionSummary>>>> {
    Ok(ok_json(
        super::summaries_of_owner(&state.web_db, Owner::Instance, true).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/git-host-connections",
    operation_id = "createInstanceGitHostConnection",
    tag = "admin",
    summary = "Create instance git host connection",
    description = "Create a Git host connection for the instance. Set `git_host_type` to `github` for an installation of a GitHub App. Pass the `installation_id` of that installation. The connection name is `github-<account>`.",
    request_body = CreateGitHostConnectionRequest,
    responses((status = OK, description = "Connection created", body = GitHostConnectionResponse), WebError)
)]
pub async fn create_instance_git_host_connection(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<CreateGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::create(&state.ci(), Owner::Instance, user.id, body).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/git-host-connections/{id}",
    operation_id = "getInstanceGitHostConnection",
    tag = "admin",
    summary = "Get instance git host connection",
    params(("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Connection", body = GitHostConnectionResponse), WebError)
)]
pub async fn get_instance_git_host_connection(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<GitHostConnectionId>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    let row = super::load_of_owner(&state.web_db, Owner::Instance, id).await?;
    Ok(ok_json(
        super::response(&state.web_db, &state.config.server.serve_url, row).await?,
    ))
}

#[utoipa::path(
    patch,
    path = "/git-host-connections/{id}",
    operation_id = "patchInstanceGitHostConnection",
    tag = "admin",
    summary = "Update instance git host connection",
    description = "Update a connection to a Git host. Omitted fields keep their value. Use an empty `access_token` or `base_url` to clear that field. Requests for GitHub App connections and for connections that declarative state manages fail.",
    params(("id" = GitHostConnectionId, Path)),
    request_body = PatchGitHostConnectionRequest,
    responses((status = OK, description = "Connection updated", body = GitHostConnectionResponse), WebError)
)]
pub async fn patch_instance_git_host_connection(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<GitHostConnectionId>,
    JsonBody(body): JsonBody<PatchGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::patch(&state.ci(), Owner::Instance, id, body).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/git-host-connections/{id}",
    operation_id = "deleteInstanceGitHostConnection",
    tag = "admin",
    summary = "Delete instance git host connection",
    description = "Delete a connection to a Git host for the instance. Removal of a connection for a GitHub App doesn't uninstall the App on GitHub. The next install webhook can add it again.",
    params(("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Deleted", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_instance_git_host_connection(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<GitHostConnectionId>,
) -> WebResult<Json<BaseResponse<bool>>> {
    super::delete(&state.web_db, Owner::Instance, id).await?;
    Ok(ok_json(true))
}

#[utoipa::path(
    get,
    path = "/git-host-connections/{id}/deliveries",
    operation_id = "listInstanceGitHostConnectionDeliveries",
    tag = "admin",
    summary = "List incoming webhook deliveries",
    description = "List the latest 50 webhook deliveries that reached the connection, newest first. Show the HTTP status and result that Gradient returned.",
    params(("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Delivery, newest first", body = Vec<IncomingWebhookDeliveryResponse>), WebError)
)]
pub async fn list_instance_git_host_connection_deliveries(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<GitHostConnectionId>,
) -> WebResult<Json<BaseResponse<Vec<IncomingWebhookDeliveryResponse>>>> {
    Ok(ok_json(
        super::deliveries(&state.web_db, Owner::Instance, id).await?,
    ))
}
