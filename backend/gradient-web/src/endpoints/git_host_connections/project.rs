/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CreateGitHostConnectionRequest, GitHostConnectionResponse, GitHostConnectionSummary,
    GitHostConnectionTaskResponse, IncomingWebhookDeliveryResponse, PatchGitHostConnectionRequest,
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
    serve_url: &str,
    project: &MProject,
    can_manage_project_connections: bool,
    repository: Option<&str>,
) -> WebResult<Vec<GitHostConnectionSummary>> {
    let rows = list_git_host_connections_available_to_project(db, project).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let owner = Owner::of(row);
        let options = super::SummaryOptions {
            serve_url,
            can_manage: owner == Owner::Project(project.id) && can_manage_project_connections,
            repository,
        };
        out.push(super::summary(db, row, super::name_owner(db, owner).await?, &options).await?);
    }
    Ok(out)
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
    description = "List the connections to Git hosts that this project can use. These groups appear in the result.\n- connections of the project\n- connections of its owner team\n- connections of the instance, with installations of the GitHub App\n\nCredentials don't appear. `can_manage` is true for the connections of the project when the caller can manage integrations. With `repository`, each connection has `repository_fit`: whether a task with this repository URL can use the connection and the reason when it can't.",
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
            &db.config.server.serve_url,
            &project,
            can_manage,
            query.repository.as_deref().filter(|r| !r.trim().is_empty()),
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
    ProjectScope { project, user, .. }: ProjectScope<RequiresUnmanaged<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<CreateGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::create(
            &state.ci(),
            Owner::Project(project.id),
            user.id,
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
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    let row =
        super::load_of_owner(&db.web_db, Owner::Project(project.id), params.param("id")?).await?;
    Ok(ok_json(
        super::response(&db.web_db, &db.config.server.serve_url, row).await?,
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
        project, params, ..
    }: ProjectScope<Requires<can::ManageIntegrations>>,
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchGitHostConnectionRequest>,
) -> WebResult<Json<BaseResponse<GitHostConnectionResponse>>> {
    Ok(ok_json(
        super::patch(
            &state.ci(),
            Owner::Project(project.id),
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
        db,
        params,
        ..
    }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<IncomingWebhookDeliveryResponse>>>> {
    let connection = load_available_to_project(&db.web_db, &project, params.param("id")?).await?;
    Ok(ok_json(
        super::deliveries_of_connection(&db.web_db, connection.id).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git-host-connections/{id}/tasks",
    operation_id = "listProjectGitHostConnectionTasks",
    tag = "git-host-connections",
    summary = "List tasks using a connection",
    description = "List the tasks that use a connection the project can use. The list holds only tasks of projects the caller can see.",
    params(ProjectPath, ("id" = GitHostConnectionId, Path)),
    responses((status = OK, description = "Tasks using the connection", body = Vec<GitHostConnectionTaskResponse>), WebError)
)]
pub async fn list_project_git_host_connection_tasks(
    ProjectScope {
        project,
        user,
        db,
        params,
        ..
    }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitHostConnectionTaskResponse>>>> {
    let connection = load_available_to_project(&db.web_db, &project, params.param("id")?).await?;
    Ok(ok_json(
        super::tasks_using_connection_visible_to_user(&db.web_db, connection.id, &user).await?,
    ))
}
