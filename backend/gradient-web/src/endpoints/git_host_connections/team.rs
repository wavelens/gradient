/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::repository_grant_list_create_delete::{
    self, CreateRepositoryGrantRequest, RepositoryGrantResponse,
};
use super::{
    CreateGitHostConnectionRequest, GitHostConnectionReaderScope, GitHostConnectionResponse,
    GitHostConnectionSummary, GitHostConnectionTaskResponse, GitHostConnectionTestResponse,
    IncomingWebhookDeliveryResponse, PatchGitHostConnectionRequest,
};
use crate::api_spec::TeamPath;
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::extractors::JsonBody;
use crate::helpers::ok_json;
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
        api_key,
        db,
        ..
    }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionSummary>>>> {
    let can_manage = user.superuser || role == Some(TeamRole::Admin);
    Ok(ok_json(
        super::summaries_of_owner(
            &db.web_db,
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            Owner::Team(team.id),
            &super::SummaryOptions {
                serve_url: &db.config.server.serve_url,
                repository: None,
            },
            can_manage,
        )
        .await?,
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
    TeamScope {
        team,
        user,
        api_key,
        ..
    }: TeamScope<AdminUnmanaged>,
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<CreateGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::create(
            &state.ci(),
            Owner::Team(team.id),
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            body,
            &audit,
        )
        .await?,
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
        team,
        user,
        api_key,
        db,
        params,
        ..
    }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    let row = super::load_of_owner(&db.web_db, Owner::Team(team.id), params.param("id")?).await?;
    Ok(ok_json(
        super::response(
            &db.web_db,
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            &db.config.server.serve_url,
            row,
        )
        .await?,
    ))
}

#[utoipa::path(
    patch,
    path = "/teams/{team}/git-host-connections/{id}",
    operation_id = "patchTeamGitHostConnection",
    tag = "git-host-connections",
    summary = "Update team git host connection",
    description = "Update a connection to a Git host. Omitted fields keep their value. Use an empty `access_token` or `base_url` to clear that field. Changes to the scheme, host, or port of `base_url` remove the stored access token, its refresh token, its expiry, and the webhook secret. Changes to `git_host_type` remove those values too. Requests store `access_token` or `secret` that they carry. Connections that receive events and have no new `secret` get a new webhook secret. Responses show that secret once in `generated_secret`. Requests for GitHub App connections and for connections that declarative state manages fail.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    request_body = PatchGitHostConnectionRequest,
    responses((status = OK, description = "Updated Connection", body = GitHostConnectionResponse), WebError)
)]
pub async fn patch_team_git_host_connection(
    TeamScope {
        team,
        user,
        api_key,
        params,
        ..
    }: TeamScope<Admin>,
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::patch(
            &state.ci(),
            Owner::Team(team.id),
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            params.param("id")?,
            body,
            &audit,
        )
        .await?,
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
    audit: Audit,
) -> WebResult<Json<BaseResponse<bool>>> {
    super::delete(
        &db.web_db,
        Owner::Team(team.id),
        params.param("id")?,
        &audit,
    )
    .await?;
    Ok(ok_json(true))
}

#[utoipa::path(
    get,
    path = "/teams/{team}/git-host-connections/{id}/repository-grants",
    operation_id = "listTeamGitHostConnectionRepositoryGrants",
    tag = "git-host-connections",
    summary = "List repository grants of a team git host connection",
    description = "List the repositories where each team project can use this team connection. Grants without `repository_name` apply to all repositories in `repository_owner`. Only a team Admin can see the list.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Repository Grants", body = Vec<RepositoryGrantResponse>), WebError)
)]
pub async fn list_team_git_host_connection_repository_grants(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<Vec<RepositoryGrantResponse>>>> {
    Ok(ok_json(
        repository_grant_list_create_delete::list(
            &db.web_db,
            Owner::Team(team.id),
            params.param("id")?,
        )
        .await?,
    ))
}

#[utoipa::path(
    put,
    path = "/teams/{team}/git-host-connections/{id}/repository-grants",
    operation_id = "createTeamGitHostConnectionRepositoryGrant",
    tag = "git-host-connections",
    summary = "Create repository grant for a team git host connection",
    description = "Tasks of `project` can use this team connection for repository `repository_owner/repository_name`. The `project` must belong to the team. Omit `repository_name` to grant all repositories of the owner. Both parts stay lowercase in Gradient. Existing grants get a 409 response.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    request_body = CreateRepositoryGrantRequest,
    responses((status = OK, description = "Created a repository grant.", body = RepositoryGrantResponse), WebError)
)]
pub async fn create_team_git_host_connection_repository_grant_for_project(
    TeamScope {
        team,
        user,
        db,
        params,
        ..
    }: TeamScope<AdminUnmanaged>,
    audit: Audit,
    JsonBody(body): JsonBody<CreateRepositoryGrantRequest>,
) -> WebResult<Json<BaseResponse<RepositoryGrantResponse>>> {
    Ok(ok_json(
        repository_grant_list_create_delete::create(
            &db.web_db,
            Owner::Team(team.id),
            params.param("id")?,
            user.id,
            body,
            &audit,
        )
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/teams/{team}/git-host-connections/{id}/repository-grants/{grant}",
    operation_id = "deleteTeamGitHostConnectionRepositoryGrant",
    tag = "git-host-connections",
    summary = "Delete repository grant of a team git host connection",
    description = "Delete repository permissions. Project tasks can no longer use connections to that repository.",
    params(TeamPath, ("id" = GitHostConnectionId, Path), ("grant" = GitHostConnectionRepositoryGrantId, Path)),
    responses((status = OK, description = "Deleted", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_team_git_host_connection_repository_grant(
    TeamScope {
        team, db, params, ..
    }: TeamScope<AdminUnmanaged>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<bool>>> {
    repository_grant_list_create_delete::delete(
        &db.web_db,
        Owner::Team(team.id),
        params.param("id")?,
        params.param("grant")?,
        &audit,
    )
    .await?;
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
        team,
        user,
        api_key,
        db,
        params,
        ..
    }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<IncomingWebhookDeliveryResponse>>>> {
    Ok(ok_json(
        super::deliveries(
            &db.web_db,
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            Owner::Team(team.id),
            params.param("id")?,
        )
        .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/teams/{team}/git-host-connections/{id}/tasks",
    operation_id = "listTeamGitHostConnectionTasks",
    tag = "git-host-connections",
    summary = "List tasks that use a connection",
    description = "List the tasks that use the team's connection. Only tasks in projects that the caller can view appear in the list.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Tasks that use a connection", body = Vec<GitHostConnectionTaskResponse>), WebError)
)]
pub async fn list_team_git_host_connection_tasks(
    TeamScope {
        team,
        user,
        api_key,
        db,
        params,
        ..
    }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionTaskResponse>>>> {
    let connection =
        super::load_of_owner(&db.web_db, Owner::Team(team.id), params.param("id")?).await?;
    Ok(ok_json(
        super::tasks_using_connection_visible_to_reader(
            &db.web_db,
            connection.id,
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
        )
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/teams/{team}/git-host-connections/{id}/test",
    operation_id = "testTeamGitHostConnection",
    tag = "git-host-connections",
    summary = "Test connection",
    description = "Sign in to the Git host with the credentials for the team connection. Return the account name for a successful sign-in. Return the reason for a failed sign-in.",
    params(TeamPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Sign-in result", body = GitHostConnectionTestResponse), WebError)
)]
pub async fn test_team_git_host_connection(
    TeamScope { team, params, .. }: TeamScope<Admin>,
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<GitHostConnectionTestResponse>>> {
    Ok(ok_json(
        super::test(&state.ci(), Owner::Team(team.id), params.param("id")?).await?,
    ))
}
