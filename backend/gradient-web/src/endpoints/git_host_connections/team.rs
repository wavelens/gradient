/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CreateGitHostConnectionRequest, GitHostConnectionResponse, GitHostConnectionSummary,
    IncomingWebhookDeliveryResponse, PatchGitHostConnectionRequest,
};
use crate::api_spec::TeamPath;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::scope::{Admin, AdminUnmanaged, Member, TeamScope};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::git_host_connections::GitHostConnectionOwnerByDatabaseId as Owner;
use gradient_types::*;
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/teams/{team}/git-host-connections",
    operation_id = "listTeamGitHostConnections",
    tag = "git-host-connections",
    summary = "List team git host connections",
    description = "List the connections to Git hosts for a team. All team projects can use them. Team members can read the list. The `can_manage` flag is true for team admins.",
    params(TeamPath),
    responses((status = OK, description = "Team Connections", body = Vec<GitHostConnectionSummary>), WebError)
)]
pub async fn list_team_git_host_connections(
    TeamScope {
        team,
        role,
        user,
        db,
        ..
    }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionSummary>>>> {
    let can_manage = user.superuser || role == Some(TeamRole::Admin);
    Ok(ok_json(
        super::summaries_of_owner(&db.web_db, Owner::Team(team.id), can_manage).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/teams/{team}/git-host-connections",
    operation_id = "createTeamGitHostConnection",
    tag = "git-host-connections",
    summary = "Create team git host connection",
    description = "Create a connection to a Git host for the team. Only a team Admin can create it. All projects that belong to the team can select that connection for tasks. `generated_secret` and `webhook_url` work the same way as for project connections.",
    params(TeamPath),
    request_body = CreateGitHostConnectionRequest,
    responses((status = OK, description = "Connection created", body = GitHostConnectionResponse), WebError)
)]
pub async fn create_team_git_host_connection(
    TeamScope { team, user, .. }: TeamScope<AdminUnmanaged>,
    State(state): State<Arc<ServerState>>,
    JsonBody(body): JsonBody<CreateGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::create(&state.ci(), Owner::Team(team.id), user.id, body).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/teams/{team}/git-host-connections/{id}",
    operation_id = "getTeamGitHostConnection",
    tag = "git-host-connections",
    summary = "Get team git host connection",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Connection", body = GitHostConnectionResponse), WebError)
)]
pub async fn get_team_git_host_connection(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    let row = super::load_of_owner(&db.web_db, Owner::Team(team.id), params.param("id")?).await?;
    Ok(ok_json(
        super::response(&db.web_db, &db.config.server.serve_url, row).await?,
    ))
}

#[utoipa::path(
    patch,
    path = "/teams/{team}/git-host-connections/{id}",
    operation_id = "patchTeamGitHostConnection",
    tag = "git-host-connections",
    summary = "Update team git host connection",
    description = "Update a connection to a Git host. Omitted fields keep their value. Use an empty `access_token` or `base_url` to clear that field. Requests for GitHub App connections and for connections that declarative state manages fail.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    request_body = PatchGitHostConnectionRequest,
    responses((status = OK, description = "Updated Connection", body = GitHostConnectionResponse), WebError)
)]
pub async fn patch_team_git_host_connection(
    TeamScope { team, params, .. }: TeamScope<Admin>,
    State(state): State<Arc<ServerState>>,
    JsonBody(body): JsonBody<PatchGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::patch(&state.ci(), Owner::Team(team.id), params.param("id")?, body).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/teams/{team}/git-host-connections/{id}",
    operation_id = "deleteTeamGitHostConnection",
    tag = "git-host-connections",
    summary = "Delete team git host connection",
    description = "Delete the connection to the Git host. Tasks that use the connection lose the connection and stop reporting commit status.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Deleted", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_team_git_host_connection(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<bool>>> {
    super::delete(&db.web_db, Owner::Team(team.id), params.param("id")?).await?;
    Ok(ok_json(true))
}

#[utoipa::path(
    get,
    path = "/teams/{team}/git-host-connections/{id}/deliveries",
    operation_id = "listTeamGitHostConnectionDeliveries",
    tag = "git-host-connections",
    summary = "List incoming webhook deliveries",
    description = "List the latest 50 webhook deliveries that reached the connection, newest first. Show the HTTP status and result that Gradient returned.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Delivery, newest first", body = Vec<IncomingWebhookDeliveryResponse>), WebError)
)]
pub async fn list_team_git_host_connection_deliveries(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<Vec<IncomingWebhookDeliveryResponse>>>> {
    Ok(ok_json(
        super::deliveries(&db.web_db, Owner::Team(team.id), params.param("id")?).await?,
    ))
}
