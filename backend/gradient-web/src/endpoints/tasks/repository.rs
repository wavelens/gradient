/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{RepositoryFetchStatus, error_message_if_caller_can_edit_task};
use crate::access::load_membership_with_permissions;
use crate::api_spec::TaskPath;
use crate::audit::Audit;
use crate::authorization::ApiKeyContext;
use crate::endpoints::git_host_connections::{
    self as connections, GitHostConnectionRefusal, GitHostConnectionSummary,
    IncomingWebhookDeliveryResponse, check_task_repository_url_can_use_git_host_connection,
};
use crate::endpoints::git_host_hooks::repo_identity;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::permissions::{Permission, mask_grants};
use crate::scope::{Member, RequiresUnmanaged, TaskScope, can};
use axum::Json;
use axum::extract::State;
use gradient_ci::task_commit_status_posting::{
    CommitStatusTestSendError,
    post_pending_test_commit_status_for_last_evaluation_and_record_attempt,
};
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_db::git_host_connections::list_git_host_connections_available_to_project;
use gradient_sources::format_public_key;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const STATUS_POST_LIST_LIMIT: u64 = 10;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct TaskCommitStatusPostAttempt {
    pub id: TaskCommitStatusPostAttemptId,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub delivered_at: chrono::NaiveDateTime,
}

impl TaskCommitStatusPostAttempt {
    fn of(
        row: gradient_entity::task_commit_status_post_attempt::Model,
        can_edit_task: bool,
    ) -> Self {
        Self {
            id: row.id,
            event: row.event,
            success: row.success,
            response_status: row.response_status,
            error_message: error_message_if_caller_can_edit_task(row.error_message, can_edit_task),
            delivered_at: row.delivered_at,
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct TaskRepositoryResponse {
    pub repository: String,
    pub uses_ssh: bool,
    pub deploy_key: Option<String>,
    pub fetch: Option<RepositoryFetchStatus>,
    pub git_host_connection: Option<GitHostConnectionSummary>,
    pub webhook_url: Option<String>,
    pub report_status: bool,
    pub last_event: Option<IncomingWebhookDeliveryResponse>,
    pub status_posts: Vec<TaskCommitStatusPostAttempt>,
    pub available_connections: Vec<GitHostConnectionSummary>,
    pub can_edit: bool,
    pub can_manage_integrations: bool,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskRepositoryConnectionAndCommitStatusPatchRequest {
    pub git_host_connection: Option<GitHostConnectionId>,
    #[serde(default)]
    pub report_status: bool,
}

async fn find_newest_incoming_webhook_delivery_for_task_repository(
    db: &impl sea_orm::ConnectionTrait,
    connection: GitHostConnectionId,
    task_repository: &str,
) -> WebResult<Option<IncomingWebhookDeliveryResponse>> {
    let task_identity = repo_identity(task_repository);
    let deliveries = gradient_db::incoming_deliveries::of_connection(
        db,
        connection,
        connections::DELIVERY_LIST_LIMIT,
    )
    .await?;
    Ok(deliveries
        .into_iter()
        .find(|d| match (&d.repository, &task_identity) {
            (Some(repository), Some(task)) => repo_identity(repository).as_ref() == Some(task),
            _ => true,
        })
        .map(Into::into))
}

async fn repository_response(
    db: &DbContext,
    project: &MProject,
    task: &MTask,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<TaskRepositoryResponse> {
    let mask = load_membership_with_permissions(&db.web_db, user.id, project.id, api_key).await?;
    let permitted = |permission| mask.is_some_and(|m| mask_grants(m, permission));
    let can_edit_task = permitted(Permission::EditTask);
    let can_manage_integrations = permitted(Permission::ManageIntegrations);

    let available_connections =
        connections::project::available_summaries(&db.web_db, project, can_manage_integrations)
            .await?;
    let git_host_connection = task
        .git_host_connection
        .and_then(|id| available_connections.iter().find(|c| c.id == id).cloned());
    let (webhook_url, last_event) = match &git_host_connection {
        Some(selected) => {
            let row = gradient_db::lookup::by_id::<EGitHostConnection, _>(&db.web_db, selected.id)
                .await?;
            let webhook_url = row
                .as_ref()
                .filter(|_| selected.can_manage)
                .and_then(|row| connections::webhook_url(&db.config.server.serve_url, row));
            let last_event = find_newest_incoming_webhook_delivery_for_task_repository(
                &db.web_db,
                selected.id,
                &task.repository,
            )
            .await?;
            (webhook_url, last_event)
        }
        None => (None, None),
    };
    let status_posts = gradient_db::git_host_connections::commit_status_post_attempts_of_task(
        &db.web_db,
        task.id,
        STATUS_POST_LIST_LIMIT,
    )
    .await?
    .into_iter()
    .map(|row| TaskCommitStatusPostAttempt::of(row, can_edit_task))
    .collect();

    let uses_ssh = check_repository_url_is_ssh(&task.repository);
    let deploy_key =
        uses_ssh.then(|| format_public_key(project.clone(), &db.config.server.serve_url));

    Ok(TaskRepositoryResponse {
        repository: task.repository.clone(),
        uses_ssh,
        deploy_key,
        fetch: RepositoryFetchStatus::of(task, can_edit_task),
        git_host_connection,
        webhook_url,
        report_status: task.report_status,
        last_event,
        status_posts,
        available_connections,
        can_edit: can_edit_task && !task.managed,
        can_manage_integrations,
    })
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/repository",
    operation_id = "getTaskRepository",
    tag = "tasks",
    summary = "Get repository connection",
    description = "Show how tasks reach a repository. Include the selected connection to the Git host, reports of commit status, latest webhook events, the last 10 status posts, and selectable connections.",
    params(TaskPath),
    responses((status = OK, description = "Repository connection", body = TaskRepositoryResponse), WebError)
)]
pub async fn get_task_repository(
    TaskScope {
        project,
        task,
        user,
        api_key,
        db,
        ..
    }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<TaskRepositoryResponse>>> {
    Ok(ok_json(
        repository_response(&db, &project, &task, &user, api_key.as_ref()).await?,
    ))
}

pub(crate) async fn check_git_host_connection_fits_task(
    db: &impl sea_orm::ConnectionTrait,
    project: &MProject,
    repository: &str,
    connection: GitHostConnectionId,
) -> WebResult<MGitHostConnection> {
    let row = list_git_host_connections_available_to_project(db, project)
        .await?
        .into_iter()
        .find(|c| c.id == connection)
        .ok_or(GitHostConnectionRefusal::NotAvailableToProject { id: connection })?;
    check_task_repository_url_can_use_git_host_connection(repository, &row).map_err(|reason| {
        GitHostConnectionRefusal::RepositoryDoesNotMatch {
            name: row.name.clone(),
            reason,
        }
    })?;
    Ok(row)
}

#[utoipa::path(
    patch,
    path = "/tasks/{project}/{task}/repository",
    operation_id = "saveTaskRepositoryConnection",
    tag = "tasks",
    summary = "Choose git host connection",
    description = "Select a connection to the Git host for the task and turn reporting of commit status on or off. Allow only connections that belong to the project, the owner team, or the instance. Two checks apply in Gradient. The repository host and the connection match. The owner name and the repository name appear in the repository URL. Reports on commit status work only on a connection with reports_status.",
    params(TaskPath),
    request_body = TaskRepositoryConnectionAndCommitStatusPatchRequest,
    responses((status = OK, description = "Repository connection", body = TaskRepositoryResponse), WebError)
)]
pub async fn save_task_repository_connection_and_commit_status(
    TaskScope {
        project,
        task,
        user,
        api_key,
        db,
        ..
    }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    audit: Audit,
    JsonBody(body): JsonBody<TaskRepositoryConnectionAndCommitStatusPatchRequest>,
) -> WebResult<Json<BaseResponse<TaskRepositoryResponse>>> {
    match body.git_host_connection {
        Some(id) => {
            let row =
                check_git_host_connection_fits_task(&db.web_db, &project, &task.repository, id)
                    .await?;
            if body.report_status && !row.reports_status {
                return Err(GitHostConnectionRefusal::ConnectionCannotReportStatus {
                    name: row.name,
                }
                .into());
            }
        }
        None if body.report_status => {
            return Err(GitHostConnectionRefusal::ReportStatusNeedsConnection.into());
        }
        None => {}
    }

    let mut active: ATask = task.into();
    active.git_host_connection = Set(body.git_host_connection);
    active.report_status = Set(body.report_status);
    let task = gradient_db::tasks::update(&db.web_db, active).await?;
    audit
        .write(
            Action::TaskRepositorySettingsReplaced,
            EventOwner::task(project.id, task.id),
            serde_json::json!({
                "git_host_connection": task.git_host_connection.map(|id| id.to_string()),
                "report_status": task.report_status,
            }),
        )
        .await;
    Ok(ok_json(
        repository_response(&db, &project, &task, &user, api_key.as_ref()).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/repository/status-test",
    operation_id = "testTaskCommitStatusReport",
    tag = "tasks",
    summary = "Test commit status report",
    description = "Post a pending commit status to the commit of the latest task evaluation. Entries in status_posts record this attempt. Return 409 for a task with no connection for status reports or no evaluated commit.",
    params(TaskPath),
    responses((status = OK, description = "Recorded Attempts", body = TaskCommitStatusPostAttempt), WebError)
)]
pub async fn post_pending_commit_status_to_head_commit_for_status_test(
    TaskScope { task, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<TaskCommitStatusPostAttempt>>> {
    let attempt =
        post_pending_test_commit_status_for_last_evaluation_and_record_attempt(&state.ci(), &task)
            .await
            .map_err(
                |error| match error.downcast_ref::<CommitStatusTestSendError>() {
                    Some(reason) => WebError::from(
                        GitHostConnectionRefusal::StatusTestNotPossible(reason.to_string()),
                    ),
                    None => WebError::Internal(error),
                },
            )?;
    Ok(ok_json(TaskCommitStatusPostAttempt::of(attempt, true)))
}
