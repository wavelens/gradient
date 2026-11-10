/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::auto_attach::integration_serves_repository;
use super::refusal::TaskRefusal;
use super::{RepositoryFetchStatus, error_message_if_caller_can_edit_task};
use crate::access::load_membership_with_permissions;
use crate::api_spec::TaskPath;
use crate::audit::Audit;
use crate::endpoints::projects::integrations::{IncomingWebhookDeliveryResponse, WebhookBase};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::permissions::{Permission, mask_grants};
use crate::scope::{Member, RequiresUnmanaged, TaskScope, can};
use axum::Json;
use gradient_ci::IntegrationKind;
use gradient_sources::format_public_key;
use gradient_types::actions::ActionConfig;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::triggers::TriggerConfig;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};

const STATUS_ACTION_NAME: &str = "Report status to Git host";

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
pub struct PushBuild {
    pub trigger_id: TaskTriggerId,
    pub integration_id: IntegrationId,
    pub active: bool,
    pub branches: Vec<String>,
    pub tags: Vec<String>,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct PullRequestBuild {
    pub trigger_id: TaskTriggerId,
    pub integration_id: IntegrationId,
    pub active: bool,
    pub branches: Vec<String>,
    pub require_approval: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
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
    pub build_on_push: Option<PushBuild>,
    pub build_on_pull_request: Option<PullRequestBuild>,
    pub last_event: Option<IncomingWebhookDeliveryResponse>,
    pub report_status: Option<StatusReport>,
    pub integrations: Vec<RepositoryIntegration>,
    pub can_edit: bool,
    pub can_manage_integrations: bool,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct UpdateTaskRepositoryRequest {
    pub inbound_integration: Option<IntegrationId>,
    #[serde(default)]
    pub build_on_push: bool,
    #[serde(default)]
    pub push_branches: Vec<String>,
    #[serde(default)]
    pub push_tags: Vec<String>,
    #[serde(default)]
    pub build_on_pull_request: bool,
    #[serde(default)]
    pub pull_request_branches: Vec<String>,
    #[serde(default = "require_approval_default")]
    pub require_approval: bool,
    pub outbound_integration: Option<IntegrationId>,
    #[serde(default)]
    pub report_status: bool,
}

fn require_approval_default() -> bool {
    true
}

struct Wiring {
    push: Vec<(MTaskTrigger, TriggerConfig)>,
    pull_request: Vec<(MTaskTrigger, TriggerConfig)>,
    status: Option<(MTaskAction, IntegrationId)>,
}

async fn load_wiring(db: &impl sea_orm::ConnectionTrait, task: TaskId) -> WebResult<Wiring> {
    let triggers = gradient_db::triggers::of_task(db, task).await?;
    let matching_of = |wanted: fn(&TriggerConfig) -> bool| {
        let mut matching: Vec<_> = triggers
            .iter()
            .filter_map(|t| {
                TriggerConfig::parse_row(t.trigger_type, &t.config)
                    .ok()
                    .filter(wanted)
                    .map(|cfg| (t.clone(), cfg))
            })
            .collect();
        matching.sort_by_key(|(t, _)| (!t.active, t.created_at));
        matching
    };
    let push = matching_of(|c| matches!(c, TriggerConfig::ReporterPush { .. }));
    let pull_request = matching_of(|c| matches!(c, TriggerConfig::ReporterPullRequest { .. }));

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

    Ok(Wiring {
        push,
        pull_request,
        status: status.into_iter().next(),
    })
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

    let wiring = load_wiring(&db.web_db, task.id).await?;
    let integrations = gradient_db::integrations::of_project(&db.web_db, project.id).await?;
    let base = WebhookBase {
        serve_url: &db.config.server.serve_url,
        project: &project.name,
    };

    let build_on_push = wiring.push.first().and_then(|(t, cfg)| match cfg {
        TriggerConfig::ReporterPush {
            integration_id,
            branches,
            tags,
            ..
        } => Some(PushBuild {
            trigger_id: t.id,
            integration_id: *integration_id,
            active: t.active,
            branches: branches.clone(),
            tags: tags.clone(),
            last_fired_at: t.last_fired_at,
        }),
        _ => None,
    });
    let build_on_pull_request = wiring.pull_request.first().and_then(|(t, cfg)| match cfg {
        TriggerConfig::ReporterPullRequest {
            integration_id,
            branches,
            require_approval,
            ..
        } => Some(PullRequestBuild {
            trigger_id: t.id,
            integration_id: *integration_id,
            active: t.active,
            branches: branches.clone(),
            require_approval: *require_approval,
            last_fired_at: t.last_fired_at,
        }),
        _ => None,
    });

    let inbound = build_on_push
        .as_ref()
        .map(|p| p.integration_id)
        .or(build_on_pull_request.as_ref().map(|p| p.integration_id));
    let last_event = match inbound {
        Some(id) => gradient_db::incoming_deliveries::of_integration(&db.web_db, id, 1)
            .await?
            .into_iter()
            .next()
            .map(Into::into),
        None => None,
    };

    let report_status = match &wiring.status {
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
                integration_id: *integration_id,
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
        build_on_push,
        build_on_pull_request,
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

fn integration_of_kind(
    integrations: &[MIntegration],
    id: Option<IntegrationId>,
    kind: IntegrationKind,
) -> WebResult<Option<IntegrationId>> {
    let Some(id) = id else { return Ok(None) };
    integrations
        .iter()
        .find(|i| i.id == id && i.kind == kind)
        .map(|i| Some(i.id))
        .ok_or_else(|| TaskRefusal::RepositoryIntegrationMismatch.into())
}

#[utoipa::path(
    put,
    path = "/tasks/{project}/{task}/repository",
    operation_id = "updateTaskRepository",
    tag = "tasks",
    summary = "Update repository connection",
    description = "Configure push and pull request builds and commit status reports for a task in one call. Gradient can sync one push trigger, one pull request trigger and one status report action per task with the request. Turn off a build or status report to disable the existing row and keep the settings. Callers need permission to manage triggers and actions. Reject tasks under state management.",
    params(TaskPath),
    request_body = UpdateTaskRepositoryRequest,
    responses((status = OK, description = "Repository connection", body = TaskRepositoryResponse), WebError)
)]
pub async fn put_task_repository(
    scope: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
    JsonBody(body): JsonBody<UpdateTaskRepositoryRequest>,
) -> WebResult<Json<BaseResponse<TaskRepositoryResponse>>> {
    let TaskScope {
        project,
        task,
        user,
        api_key,
        db,
        ..
    } = &scope;
    crate::access::require_project_permission(
        &db.web_db,
        user.id,
        project.id,
        Permission::ManageActions,
        crate::resource::Resource::Task,
        api_key.as_ref(),
    )
    .await?;

    let integrations = gradient_db::integrations::of_project(&db.web_db, project.id).await?;
    let inbound = integration_of_kind(
        &integrations,
        body.inbound_integration,
        IntegrationKind::Inbound,
    )?;
    let outbound = integration_of_kind(
        &integrations,
        body.outbound_integration,
        IntegrationKind::Outbound,
    )?;
    if (body.build_on_push || body.build_on_pull_request) && inbound.is_none() {
        return Err(TaskRefusal::RepositoryInboundRequired.into());
    }
    if body.report_status && outbound.is_none() {
        return Err(TaskRefusal::RepositoryOutboundRequired.into());
    }

    let txn = db.web_db.inner().begin().await?;
    gradient_db::lookup::by_id_for_delete::<ETask, _>(&txn, task.id)
        .await?
        .ok_or_else(|| WebError::not_found(crate::resource::Resource::Task))?;
    let wiring = load_wiring(&txn, task.id).await?;
    let now = gradient_types::now();

    let push = inbound
        .filter(|_| body.build_on_push)
        .map(|integration_id| {
            let (releases_only, include_flake_inputs) = match wiring.push.first() {
                Some((
                    _,
                    TriggerConfig::ReporterPush {
                        releases_only,
                        include_flake_inputs,
                        ..
                    },
                )) => (*releases_only, *include_flake_inputs),
                _ => (false, false),
            };
            TriggerConfig::ReporterPush {
                integration_id,
                branches: clean(&body.push_branches),
                tags: clean(&body.push_tags),
                releases_only,
                include_flake_inputs,
            }
        });
    sync_trigger(
        &txn,
        task.id,
        wiring.push.into_iter().map(|(t, _)| t).collect(),
        push,
        now,
    )
    .await?;

    let pull_request = inbound
        .filter(|_| body.build_on_pull_request)
        .map(|integration_id| {
            let actions = match wiring.pull_request.first() {
                Some((_, TriggerConfig::ReporterPullRequest { actions, .. })) => actions.clone(),
                _ => ["opened", "synchronize", "reopened"]
                    .map(String::from)
                    .to_vec(),
            };
            TriggerConfig::ReporterPullRequest {
                integration_id,
                branches: clean(&body.pull_request_branches),
                actions,
                require_approval: body.require_approval,
            }
        });
    sync_trigger(
        &txn,
        task.id,
        wiring.pull_request.into_iter().map(|(t, _)| t).collect(),
        pull_request,
        now,
    )
    .await?;

    let status = outbound
        .filter(|_| body.report_status)
        .map(|integration_id| ActionConfig::GitHostStatusReport { integration_id });
    sync_status_action(&txn, task, wiring.status.map(|(a, _)| a), status, now).await?;
    txn.commit().await?;

    let response = repository_response(&scope).await?;
    audit
        .write(
            Action::TaskRepositorySettingsReplaced,
            EventOwner::task(project.id, task.id),
            serde_json::json!({
                "inbound_integration": inbound.map(|id| id.to_string()),
                "outbound_integration": outbound.map(|id| id.to_string()),
                "build_on_push": body.build_on_push,
                "build_on_pull_request": body.build_on_pull_request,
                "report_status": body.report_status,
            }),
        )
        .await;
    Ok(ok_json(response))
}

fn clean(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn sync_trigger(
    db: &impl sea_orm::ConnectionTrait,
    task: TaskId,
    existing: Vec<MTaskTrigger>,
    desired: Option<TriggerConfig>,
    now: chrono::NaiveDateTime,
) -> WebResult<()> {
    let mut existing = existing.into_iter();
    match (existing.next(), desired) {
        (Some(row), Some(cfg)) => {
            let mut active: ATaskTrigger = row.into();
            active.config = Set(cfg.to_db_json());
            active.active = Set(true);
            active.updated_at = Set(now);
            gradient_db::triggers::update(db, active).await?;
        }
        (Some(row), None) => {
            for row in std::iter::once(row).chain(existing).filter(|t| t.active) {
                let mut active: ATaskTrigger = row.into();
                active.active = Set(false);
                active.updated_at = Set(now);
                gradient_db::triggers::update(db, active).await?;
            }
        }
        (None, Some(cfg)) => {
            gradient_db::triggers::insert(
                db,
                MTaskTrigger {
                    id: TaskTriggerId::now_v7(),
                    task,
                    trigger_type: cfg.trigger_type(),
                    config: cfg.to_db_json(),
                    active: true,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                },
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

async fn sync_status_action(
    db: &impl sea_orm::ConnectionTrait,
    task: &MTask,
    existing: Option<MTaskAction>,
    desired: Option<ActionConfig>,
    now: chrono::NaiveDateTime,
) -> WebResult<()> {
    match (existing, desired) {
        (Some(row), desired) if desired.is_some() || row.active => {
            let read_at = row.updated_at;
            let mut active: ATaskAction = row.into();
            if let Some(cfg) = &desired {
                active.config = Set(cfg.to_db_json());
            }
            active.active = Set(desired.is_some());
            active.updated_at = Set(now);
            gradient_db::actions::update(db, active, read_at).await?;
        }
        (None, Some(cfg)) => {
            gradient_db::actions::insert(
                db,
                MTaskAction {
                    id: TaskActionId::now_v7(),
                    task: task.id,
                    name: STATUS_ACTION_NAME.into(),
                    action_type: cfg.action_type(),
                    config: cfg.to_db_json(),
                    events: serde_json::json!([]),
                    active: true,
                    created_by: task.created_by,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                },
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}
