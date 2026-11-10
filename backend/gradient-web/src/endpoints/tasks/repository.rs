/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::integration_by_repository_host::integration_serves_repository;
use super::{RepositoryFetchStatus, error_message_if_caller_can_edit_task};
use crate::access::load_membership_with_permissions;
use crate::api_spec::TaskPath;
use crate::audit::Audit;
use crate::endpoints::projects::integrations::{IncomingWebhookDeliveryResponse, WebhookBase};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::{Permission, mask_grants};
use crate::scope::{Member, TaskScope};
use axum::Json;
use gradient_ci::IntegrationKind;
use gradient_sources::format_public_key;
use gradient_types::actions::ActionConfig;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::triggers::TriggerConfig;
use gradient_types::*;
use serde::Serialize;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct RepositoryIntegration {
    pub id: IntegrationId,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub git_host_type: String,
    pub serves_repository: bool,
    pub webhook_url: Option<String>,
    pub has_secret: Option<bool>,
    pub has_access_token: Option<bool>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct StatusReportDelivery {
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub delivered_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct StatusReport {
    pub action_id: TaskActionId,
    pub integration_id: IntegrationId,
    pub active: bool,
    pub last_delivery: Option<StatusReportDelivery>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct TaskRepositoryResponse {
    pub repository: String,
    pub uses_ssh: bool,
    pub deploy_key: Option<String>,
    pub fetch: Option<RepositoryFetchStatus>,
    pub last_event: Option<IncomingWebhookDeliveryResponse>,
    pub report_status: Option<StatusReport>,
    pub integrations: Vec<RepositoryIntegration>,
    pub can_edit: bool,
    pub can_manage_integrations: bool,
}

async fn first_push_or_pull_request_integration(
    db: &impl sea_orm::ConnectionTrait,
    task: TaskId,
) -> WebResult<Option<IntegrationId>> {
    let mut triggers = gradient_db::triggers::of_task(db, task).await?;
    triggers.sort_by_key(|t| (!t.active, t.created_at));
    Ok(triggers.iter().find_map(|t| {
        match TriggerConfig::parse_row(t.trigger_type, &t.config).ok()? {
            TriggerConfig::ReporterPush { integration_id, .. }
            | TriggerConfig::ReporterPullRequest { integration_id, .. } => Some(integration_id),
            _ => None,
        }
    }))
}

async fn status_report_action(
    db: &impl sea_orm::ConnectionTrait,
    task: TaskId,
) -> WebResult<Option<(MTaskAction, IntegrationId)>> {
    let mut status: Vec<_> = gradient_db::actions::of_task(db, task)
        .await?
        .into_iter()
        .filter_map(
            |a| match serde_json::from_value::<ActionConfig>(a.config.clone()) {
                Ok(ActionConfig::GitHostStatusReport { integration_id }) => {
                    Some((a, integration_id))
                }
                _ => None,
            },
        )
        .collect();
    status.sort_by_key(|(a, _)| (!a.active, a.created_at));
    Ok(status.into_iter().next())
}

async fn repository_response(
    TaskScope {
        project,
        task,
        user,
        api_key,
        db,
        ..
    }: &TaskScope<impl crate::scope::TaskRule<User = MUser>>,
) -> WebResult<TaskRepositoryResponse> {
    let mask =
        load_membership_with_permissions(&db.web_db, user.id, project.id, api_key.as_ref()).await?;
    let permitted = |permission| mask.is_some_and(|m| mask_grants(m, permission));
    let can_edit_task = permitted(Permission::EditTask);
    let can_edit = !task.managed
        && permitted(Permission::ManageTriggers)
        && permitted(Permission::ManageActions);
    let can_manage_integrations = permitted(Permission::ManageIntegrations);

    let trigger_integration = first_push_or_pull_request_integration(&db.web_db, task.id).await?;
    let status_action = status_report_action(&db.web_db, task.id).await?;
    let integrations = gradient_db::integrations::of_project(&db.web_db, project.id).await?;
    let base = WebhookBase {
        serve_url: &db.config.server.serve_url,
        project: &project.name,
    };

    let inbound = trigger_integration.or_else(|| {
        integrations
            .iter()
            .find(|i| {
                i.kind == IntegrationKind::Inbound
                    && integration_serves_repository(&task.repository, i, &integrations)
            })
            .map(|i| i.id)
    });
    let last_event = match inbound {
        Some(id) => gradient_db::incoming_deliveries::of_integration(&db.web_db, id, 1)
            .await?
            .into_iter()
            .next()
            .map(Into::into),
        None => None,
    };

    let report_status = match status_action {
        Some((action, integration_id)) => {
            let last_delivery = gradient_db::actions::deliveries_of(&db.web_db, action.id, 1, 0)
                .await?
                .into_iter()
                .next()
                .map(|d| StatusReportDelivery {
                    success: d.success,
                    response_status: d.response_status,
                    error_message: error_message_if_caller_can_edit_task(
                        d.error_message,
                        can_edit_task,
                    ),
                    delivered_at: d.delivered_at,
                });
            Some(StatusReport {
                action_id: action.id,
                integration_id,
                active: action.active,
                last_delivery,
            })
        }
        None => None,
    };

    let uses_ssh = check_repository_url_is_ssh(&task.repository);
    let deploy_key =
        uses_ssh.then(|| format_public_key(project.clone(), &db.config.server.serve_url));

    let integrations = integrations
        .iter()
        .map(|i| RepositoryIntegration {
            id: i.id,
            name: i.name.clone(),
            display_name: i.display_name.clone(),
            kind: match i.kind {
                IntegrationKind::Inbound => "inbound".into(),
                IntegrationKind::Outbound => "outbound".into(),
            },
            git_host_type: i.git_host_type.as_path_segment().to_string(),
            serves_repository: integration_serves_repository(&task.repository, i, &integrations),
            webhook_url: base.url_for(i),
            has_secret: can_manage_integrations.then_some(i.secret.is_some()),
            has_access_token: can_manage_integrations.then_some(i.access_token.is_some()),
        })
        .collect();

    Ok(TaskRepositoryResponse {
        repository: task.repository.clone(),
        uses_ssh,
        deploy_key,
        fetch: RepositoryFetchStatus::of(task, can_edit_task),
        last_event,
        report_status,
        integrations,
        can_edit,
        can_manage_integrations,
    })
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/repository",
    operation_id = "getTaskRepository",
    tag = "tasks",
    summary = "Get repository connection",
    description = "Description Retrieval",
    params(TaskPath),
    responses((status = OK, description = "Repository connection", body = TaskRepositoryResponse), WebError)
)]
pub async fn get_task_repository(
    scope: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<TaskRepositoryResponse>>> {
    Ok(ok_json(repository_response(&scope).await?))
}
