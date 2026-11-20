/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    ConnectionToSummarize, CreateGitHostConnectionRequest, GitHostConnectionReaderScope,
    GitHostConnectionResponse, GitHostConnectionSummary, GitHostConnectionTaskResponse,
    GitHostConnectionTestResponse, IncomingWebhookDeliveryResponse, PatchGitHostConnectionRequest,
    SummaryOptions,
};
use crate::access::load_membership_with_permissions;
use crate::api_spec::ProjectPath;
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::extractors::{JsonBody, Query};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{Permission, mask_grants};
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Requires, RequiresUnmanaged, can};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::git_host_connections::{
    GitHostConnectionOwnerByDatabaseId as Owner, list_git_host_connections_available_to_project,
};
use gradient_types::*;
use serde::Deserialize;
use std::sync::Arc;

pub(crate) async fn available_summaries(
    db: &impl sea_orm::ConnectionTrait,
    reader: &GitHostConnectionReaderScope<'_>,
    options: &SummaryOptions<'_>,
    project: &MProject,
    can_manage_project_connections: bool,
) -> WebResult<Vec<GitHostConnectionSummary>> {
    let rows = list_git_host_connections_available_to_project(db, project)
        .await?
        .into_iter()
        .map(|connection| ConnectionToSummarize {
            can_manage: Owner::of(&connection) == Owner::Project(project.id)
                && can_manage_project_connections,
            connection,
        })
        .collect();
    super::summaries(db, reader, options, rows).await
}

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProjectGitHostConnectionListQuery {
    pub repository: Option<String>,
}

pub(crate) async fn load_available_to_project(
    db: &impl sea_orm::ConnectionTrait,
    project: &MProject,
    id: GitHostConnectionId,
) -> WebResult<MGitHostConnection> {
    list_git_host_connections_available_to_project(db, project)
        .await?
        .into_iter()
        .find(|row| row.id == id)
        .or_not_found(Resource::GitHostConnection)
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git-host-connections",
    operation_id = "listProjectGitHostConnections",
    tag = "git-host-connections",
    summary = "List usable git host connections",
    description = "List the connections to Git hosts that this project can use. These groups appear in the result.\n- connections of the project\n- connections of its owner team\n- connections of the instance, with installations of the GitHub App\n\nCredentials don't appear. `can_manage` is true for the connections of the project when the caller can manage integrations. All connections get `repository_fit` with `repository` in the request. These values report whether a task with that repository URL can use the connection and the reason when the task can't.",
    params(ProjectPath, ProjectGitHostConnectionListQuery),
    responses((status = OK, description = "Usable connections", body = Vec<GitHostConnectionSummary>), WebError)
)]
pub async fn list_project_git_host_connections(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        ..
    }: ProjectScope<Member>,
    Query(query): Query<ProjectGitHostConnectionListQuery>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionSummary>>>> {
    let mask =
        load_membership_with_permissions(&db.web_db, user.id, project.id, api_key.as_ref()).await?;
    let can_manage = mask.is_some_and(|m| mask_grants(m, Permission::ManageIntegrations));
    Ok(ok_json(
        available_summaries(
            &db.web_db,
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            &SummaryOptions {
                serve_url: &db.config.server.serve_url,
                repository: query.repository.as_deref().filter(|r| !r.trim().is_empty()),
            },
            &project,
            can_manage,
        )
        .await?,
    ))
}

#[utoipa::path(
    put,
    path = "/projects/{project}/git-host-connections",
    operation_id = "createProjectGitHostConnection",
    tag = "git-host-connections",
    summary = "Create project git host connection",
    description = "Create a connection to a Git host for this project. The Git host can send webhooks to `webhook_url` with `receives_events`. The server can generate `secret` if absent and return the new value once in `generated_secret`. Use `reports_status` to post commit statuses with `base_url` and `access_token`. GitHub repositories use the GitHub App of the instance instead.",
    params(ProjectPath),
    request_body = CreateGitHostConnectionRequest,
    responses((status = OK, description = "Connection created", body = GitHostConnectionResponse), WebError)
)]
pub async fn create_project_git_host_connection(
    ProjectScope {
        project,
        user,
        api_key,
        ..
    }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<CreateGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::create(
            &state.ci(),
            Owner::Project(project.id),
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            body,
            &audit,
        )
        .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git-host-connections/{id}",
    operation_id = "getProjectGitHostConnection",
    tag = "git-host-connections",
    summary = "Get project git host connection",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Connection", body = GitHostConnectionResponse), WebError)
)]
pub async fn get_project_git_host_connection(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    let row =
        super::load_of_owner(&db.web_db, Owner::Project(project.id), params.param("id")?).await?;
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
    path = "/projects/{project}/git-host-connections/{id}",
    operation_id = "patchProjectGitHostConnection",
    tag = "git-host-connections",
    summary = "Update project git host connection",
    description = "Update a connection to a Git host. Omitted fields keep their value. Use an empty `access_token` or `base_url` to clear that field. Requests for GitHub App connections and for connections that declarative state manages fail.",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    request_body = PatchGitHostConnectionRequest,
    responses((status = OK, description = "Updated connection", body = GitHostConnectionResponse), WebError)
)]
pub async fn patch_project_git_host_connection(
    ProjectScope {
        project,
        user,
        api_key,
        params,
        ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::patch(
            &state.ci(),
            Owner::Project(project.id),
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
    path = "/projects/{project}/git-host-connections/{id}",
    operation_id = "deleteProjectGitHostConnection",
    tag = "git-host-connections",
    summary = "Delete project git host connection",
    description = "Delete the connection to the Git host. Tasks that use the connection lose the connection and stop reporting commit status.",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Deleted", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_project_git_host_connection(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<bool>>> {
    super::delete(
        &db.web_db,
        Owner::Project(project.id),
        params.param("id")?,
        &audit,
    )
    .await?;
    Ok(ok_json(true))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git-host-connections/{id}/deliveries",
    operation_id = "listProjectGitHostConnectionDeliveries",
    tag = "git-host-connections",
    summary = "List incoming webhook deliveries",
    description = "List the latest 50 webhook deliveries that reached the connection, newest first. Show the HTTP status and result that Gradient returned.",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Deliveries, newest first", body = Vec<IncomingWebhookDeliveryResponse>), WebError)
)]
pub async fn list_project_git_host_connection_deliveries(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        params,
        ..
    }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<IncomingWebhookDeliveryResponse>>>> {
    let connection = load_available_to_project(&db.web_db, &project, params.param("id")?).await?;
    Ok(ok_json(
        super::deliveries_of_connection(
            &db.web_db,
            &GitHostConnectionReaderScope::new(&user, api_key.as_ref()),
            &connection,
        )
        .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git-host-connections/{id}/tasks",
    operation_id = "listProjectGitHostConnectionTasks",
    tag = "git-host-connections",
    summary = "List tasks that use a connection",
    description = "List the tasks that use a connection available to the project. The list includes only tasks in projects that the caller can view.",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Tasks that use a connection", body = Vec<GitHostConnectionTaskResponse>), WebError)
)]
pub async fn list_project_git_host_connection_tasks(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        params,
        ..
    }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionTaskResponse>>>> {
    let connection = load_available_to_project(&db.web_db, &project, params.param("id")?).await?;
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
    path = "/projects/{project}/git-host-connections/{id}/test",
    operation_id = "testProjectGitHostConnection",
    tag = "git-host-connections",
    summary = "Test connection",
    description = "Sign in to the Git host with the credentials from the project connection. Return the account name on success. Return the reason on failure.",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Sign-in result", body = GitHostConnectionTestResponse), WebError)
)]
pub async fn test_project_git_host_connection(
    ProjectScope {
        project, params, ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<GitHostConnectionTestResponse>>> {
    Ok(ok_json(
        super::test(&state.ci(), Owner::Project(project.id), params.param("id")?).await?,
    ))
}
