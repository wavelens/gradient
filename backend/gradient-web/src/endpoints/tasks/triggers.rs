/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DeletedResponse;
use super::integration_by_repository_host::suggest_push_and_pull_request_triggers_for_repository_url;
use super::refusal::TaskRefusal;
use crate::api_spec::{ProjectPath, TaskPath};
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Requires, RequiresUnmanaged, TaskScope, can};
use anyhow::Context;
use axum::extract::Query;
use axum::{Extension, Json};
use chrono::Utc;
use gradient_ci::IntegrationKind;
use gradient_ci::trigger::InputUpdateOutcome;
use gradient_ci::{ApplyInput, ApplyOutcome, apply_trigger};
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_db::lookup::by_ids;
use gradient_entity::evaluation::WalkMode;
use gradient_scheduler::Scheduler;
use gradient_sources::head_commit;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn router() -> OpenApiRouter<Arc<ServerState>> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(read, update, delete_one))
        .routes(routes!(fire_now))
        .routes(routes!(set_default))
}

#[derive(Serialize, Debug, ToSchema)]
pub struct TriggerIntegrationSummary {
    pub id: IntegrationId,
    pub name: String,
    pub display_name: String,
    pub git_host_type: String,
}

impl From<&MIntegration> for TriggerIntegrationSummary {
    fn from(m: &MIntegration) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            display_name: m.display_name.clone(),
            git_host_type: m.git_host_type.as_path_segment().to_string(),
        }
    }
}

#[derive(Serialize, Debug, ToSchema)]
pub struct TriggerOut {
    pub id: TaskTriggerId,
    pub task: TaskId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    pub config: serde_json::Value,
    pub active: bool,
    pub is_default: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
    pub integration: Option<TriggerIntegrationSummary>,
}

impl TriggerOut {
    fn build(m: MTaskTrigger, integrations: &HashMap<IntegrationId, MIntegration>) -> Self {
        let integration = trigger_integration_id(m.trigger_type, &m.config)
            .and_then(|id| integrations.get(&id))
            .map(TriggerIntegrationSummary::from);
        Self {
            id: m.id,
            task: m.task,
            trigger_type: m.trigger_type,
            config: m.config,
            active: m.active,
            is_default: m.is_default,
            last_fired_at: m.last_fired_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
            integration,
        }
    }
}

pub(super) fn trigger_integration_id(
    trigger_type: TriggerType,
    config: &serde_json::Value,
) -> Option<IntegrationId> {
    TriggerConfig::parse_row(trigger_type, config)
        .ok()
        .as_ref()
        .and_then(trigger_integration_id_of)
}

fn trigger_integration_id_of(config: &TriggerConfig) -> Option<IntegrationId> {
    match config {
        TriggerConfig::ReporterPush { integration_id, .. }
        | TriggerConfig::ReporterPullRequest { integration_id, .. } => Some(*integration_id),
        _ => None,
    }
}

fn build_trigger_audit_log_json(trigger: &MTaskTrigger) -> serde_json::Value {
    serde_json::json!({
        "trigger_id": trigger.id.to_string(),
        "trigger_type": trigger.trigger_type,
        "active": trigger.active,
    })
}

async fn load_integrations_for_triggers<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    rows: &[MTaskTrigger],
) -> WebResult<HashMap<IntegrationId, MIntegration>> {
    let ids: Vec<IntegrationId> = rows
        .iter()
        .filter_map(|t| trigger_integration_id(t.trigger_type, &t.config))
        .collect();
    let mut integrations = by_ids::<EIntegration, _>(db, &ids).await?;
    integrations.retain(|_, integration| integration.project == project);
    Ok(integrations)
}

#[derive(Deserialize, Debug, ToSchema)]
pub struct CreateTriggerRequest {
    pub config: TriggerConfig,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug, ToSchema)]
pub struct UpdateTriggerRequest {
    pub config: Option<TriggerConfig>,
    pub active: Option<bool>,
}

#[derive(Deserialize, Debug, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TriggerSuggestionsQuery {
    pub repository: String,
}

#[utoipa::path(
    get,
    path = "/projects/{project}/trigger-suggestions",
    operation_id = "listTriggerSuggestions",
    tag = "triggers",
    summary = "Suggest push and pull request triggers",
    description = "Return the push triggers and pull request triggers that tasks with `repository` can add. Gradient can build them from the project's webhook receiver for that Git host. The list is empty if no webhook receiver can handle the host, or if several can. Gradient doesn't save the suggestions. Use the endpoint for task creation or the endpoint for trigger creation to create them.",
    params(ProjectPath, TriggerSuggestionsQuery),
    responses((status = OK, description = "Recommended Triggers", body = Vec<TriggerConfig>), WebError)
)]
pub async fn get_project_repository_trigger_suggestions(
    ProjectScope { project, db, .. }: ProjectScope<Member>,
    Query(query): Query<TriggerSuggestionsQuery>,
) -> WebResult<Json<BaseResponse<Vec<TriggerConfig>>>> {
    let integrations = gradient_db::integrations::of_project(&db.web_db, project.id).await?;
    Ok(ok_json(
        suggest_push_and_pull_request_triggers_for_repository_url(&query.repository, &integrations),
    ))
}

#[utoipa::path(
    get,
    path = "/",
    operation_id = "listTaskTriggers",
    tag = "triggers",
    summary = "List task triggers",
    params(TaskPath),
    responses((status = OK, description = "List of triggers", body = Vec<TriggerOut>), WebError)
)]
pub async fn list(
    TaskScope { task: proj, db, .. }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<TriggerOut>>>> {
    let rows = gradient_db::triggers::of_task(&db.web_db, proj.id).await?;

    let integrations = load_integrations_for_triggers(&db.web_db, proj.project, &rows).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(|r| TriggerOut::build(r, &integrations))
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/",
    operation_id = "createTaskTrigger",
    tag = "triggers",
    summary = "Create task trigger",
    params(TaskPath),
    request_body = CreateTriggerRequest,
    responses((status = OK, description = "Trigger created", body = TriggerOut), WebError)
)]
pub async fn create(
    TaskScope {
        project,
        task: proj,
        db,
        ..
    }: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
    Json(body): Json<CreateTriggerRequest>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let config =
        validate_trigger_config_and_return_config_to_store(&db.web_db, project.id, body.config)
            .await?;
    let trigger = build_task_trigger_row_from_config_and_flags(
        proj.id,
        &config,
        body.active,
        false,
        gradient_types::now(),
    );
    let row = gradient_db::triggers::insert(&db.web_db, trigger).await?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, proj.project, std::slice::from_ref(&row))
            .await?;
    audit
        .write(
            Action::TaskEvaluationTriggerCreate,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&row),
        )
        .await;
    Ok(ok_json(TriggerOut::build(row, &integrations)))
}

pub(crate) async fn validate_trigger_config_and_return_config_to_store<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    config: TriggerConfig,
) -> WebResult<TriggerConfig> {
    let config = config
        .validate_and_convert_cron_to_six_fields()
        .map_err(|e| TaskRefusal::InvalidTriggerConfig(e.to_string()))?;
    let Some(integration_id) = trigger_integration_id_of(&config) else {
        return Ok(config);
    };
    let inbound = gradient_db::integrations::of_project(db, project)
        .await?
        .iter()
        .any(|i| i.id == integration_id && i.kind == IntegrationKind::Inbound);
    if !inbound {
        return Err(TaskRefusal::TriggerIntegrationNotInbound.into());
    }
    Ok(config)
}

pub(crate) fn build_task_trigger_row_from_config_and_flags(
    task: TaskId,
    config: &TriggerConfig,
    active: bool,
    is_default: bool,
    now: chrono::NaiveDateTime,
) -> MTaskTrigger {
    MTaskTrigger {
        id: TaskTriggerId::now_v7(),
        task,
        trigger_type: config.trigger_type(),
        config: config.to_db_json(),
        active,
        is_default,
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
}

#[utoipa::path(
    get,
    path = "/{id}",
    operation_id = "getTaskTrigger",
    tag = "triggers",
    summary = "Get task trigger",
    params(TaskPath, ("id" = TaskTriggerId, Path)),
    responses((status = OK, description = "Trigger", body = TriggerOut), WebError)
)]
pub async fn read(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = gradient_db::triggers::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Trigger)?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, proj.project, std::slice::from_ref(&row))
            .await?;
    Ok(ok_json(TriggerOut::build(row, &integrations)))
}

#[utoipa::path(
    patch,
    path = "/{id}",
    operation_id = "updateTaskTrigger",
    tag = "triggers",
    summary = "Update task trigger",
    params(TaskPath, ("id" = TaskTriggerId, Path)),
    request_body = UpdateTriggerRequest,
    responses((status = OK, description = "Trigger updated", body = TriggerOut), WebError)
)]
pub async fn update(
    TaskScope {
        project,
        task: proj,
        db,
        params,
        ..
    }: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
    Json(body): Json<UpdateTriggerRequest>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = gradient_db::triggers::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Trigger)?;

    let config = match body.config {
        Some(cfg) => Some(
            validate_trigger_config_and_return_config_to_store(&db.web_db, project.id, cfg).await?,
        ),
        None => None,
    };

    let mut active: ATaskTrigger = row.into();
    if let Some(cfg) = config {
        let tt = cfg.trigger_type();
        active.trigger_type = Set(tt);
        active.config = Set(cfg.to_db_json());
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = gradient_db::triggers::update(&db.web_db, active).await?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, proj.project, std::slice::from_ref(&updated))
            .await?;
    audit
        .write(
            Action::TaskEvaluationTriggerChanged,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&updated),
        )
        .await;
    Ok(ok_json(TriggerOut::build(updated, &integrations)))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    operation_id = "deleteTaskTrigger",
    tag = "triggers",
    summary = "Delete task trigger",
    params(TaskPath, ("id" = TaskTriggerId, Path)),
    responses((status = OK, description = "Trigger deleted", body = DeletedResponse), WebError)
)]
pub async fn delete_one(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = gradient_db::triggers::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Trigger)?;

    if row.is_default {
        return Err(TaskRefusal::DefaultTriggerDeletion.into());
    }

    let metadata = build_trigger_audit_log_json(&row);
    gradient_db::triggers::delete(&db.web_db, row).await?;
    audit
        .write(
            Action::TaskEvaluationTriggerDelete,
            EventOwner::task(proj.project, proj.id),
            metadata,
        )
        .await;

    Ok(ok_json(DeletedResponse::deleted()))
}

#[utoipa::path(
    post,
    path = "/{id}/default",
    operation_id = "setDefaultTaskTrigger",
    tag = "triggers",
    summary = "Make trigger the default",
    description = "The default trigger decides the branch of `POST /tasks/{project}/{task}/evaluate`. The previous default trigger stops being the default.",
    params(TaskPath, ("id" = TaskTriggerId, Path)),
    responses((status = OK, description = "Trigger updated", body = TriggerOut), WebError)
)]
pub async fn set_default(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = gradient_db::triggers::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Trigger)?;

    let txn = db.web_db.begin().await?;
    let updated = gradient_db::triggers::make_default(&txn, row).await?;
    txn.commit().await?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, proj.project, std::slice::from_ref(&updated))
            .await?;
    audit
        .write(
            Action::TriggerBecameDefaultForTask,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&updated),
        )
        .await;
    Ok(ok_json(TriggerOut::build(updated, &integrations)))
}

#[derive(Serialize, Debug, ToSchema)]
#[serde(tag = "outcome")]
pub enum FireOutcome {
    Created { evaluation_id: EvaluationId },
    SkippedSameCommit,
    SkippedConcurrency,
    InputUpdateRunning,
    InputUpdateStarted { evaluation_ids: Vec<EvaluationId> },
}

pub(super) struct FireRequest {
    pub started_by: Option<UserId>,
    pub walk_mode: WalkMode,
    pub wildcard_override: Option<String>,
    pub counts_as_trigger_run: bool,
}

#[utoipa::path(
    post,
    path = "/{id}/test",
    operation_id = "testTaskTrigger",
    tag = "triggers",
    summary = "Manually fire trigger",
    description = "Resolves the task HEAD and applies the trigger immediately, bypassing commit dedup. A task with an active `open_pr` action and tracked flake inputs starts only its input update. Returns the outcome.",
    params(TaskPath, ("id" = TaskTriggerId, Path)),
    responses((status = OK, description = "Trigger apply outcome", body = FireOutcome), WebError)
)]
pub async fn fire_now(
    TaskScope {
        task: proj,
        user,
        db,
        params,
        ..
    }: TaskScope<Requires<can::TriggerEvaluation>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<FireOutcome>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = gradient_db::triggers::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Trigger)?;

    if !row.active {
        return Err(TaskRefusal::InactiveTrigger.into());
    }

    let request = FireRequest {
        started_by: Some(user.id),
        walk_mode: WalkMode::Pruned,
        wildcard_override: None,
        counts_as_trigger_run: true,
    };
    Ok(ok_json(fire(&db, &scheduler, &proj, &row, request).await?))
}

pub(super) async fn fire(
    db: &DbContext,
    scheduler: &Scheduler,
    task: &MTask,
    trigger: &MTaskTrigger,
    request: FireRequest,
) -> WebResult<FireOutcome> {
    let branch = TriggerConfig::parse_row(trigger.trigger_type, &trigger.config)
        .ok()
        .and_then(|cfg| cfg.trigger_branch().map(str::to_owned));

    let (branch, head) = head_commit(db, task, branch.as_deref())
        .await
        .map_err(|e| {
            tracing::warn!(task_id = %task.id, trigger_id = %trigger.id, error = %e, "rejecting evaluation: failed to fetch the head commit of the repository");
            SharedRefusal::RepositoryStateFetchFailed
        })?;

    let trigger_id = request.counts_as_trigger_run.then_some(trigger.id);
    let input_update = gradient_ci::trigger::maybe_trigger_input_update(
        &db.web_db,
        task,
        head.hash.clone(),
        trigger_id,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, trigger_id = %trigger.id, "manual input_update trigger failed");
        InputUpdateOutcome::NotConfigured
    });

    let outcome = match input_update {
        InputUpdateOutcome::NotConfigured => {
            let input = ApplyInput {
                trigger_id,
                trigger_type: trigger.trigger_type,
                branch,
                commit_hash: head.hash,
                commit_message: Some(head.message),
                author_name: Some(head.author_name),
                manual: true,
                honors_skip_marker: false,
                gate_approval: None,
                repository_override: None,
                wildcard_override: request.wildcard_override,
                source_comment: None,
                started_by: request.started_by,
                walk_mode: request.walk_mode,
            };
            apply_now(db, scheduler, task, input).await?
        }
        InputUpdateOutcome::AlreadyRunning => FireOutcome::InputUpdateRunning,
        InputUpdateOutcome::Started(evaluation_ids) => {
            FireOutcome::InputUpdateStarted { evaluation_ids }
        }
    };

    if request.counts_as_trigger_run {
        stamp_last_fired(&db.web_db, trigger).await;
    }

    Ok(outcome)
}

async fn apply_now(
    db: &DbContext,
    scheduler: &Scheduler,
    task: &MTask,
    input: ApplyInput,
) -> WebResult<FireOutcome> {
    let outcome = apply_trigger(&db.web_db, task, input)
        .await
        .context("apply the trigger")?;

    Ok(match outcome {
        ApplyOutcome::Created {
            evaluation: eval,
            aborted_evaluation,
            hard_abort,
        } => {
            if let Some(aborted_id) = aborted_evaluation {
                scheduler
                    .stop_aborted_evaluation(aborted_id, hard_abort)
                    .await;
            }

            db.record_evaluation_created(&eval).await;
            FireOutcome::Created {
                evaluation_id: eval.id,
            }
        }
        ApplyOutcome::SkippedSameCommit => FireOutcome::SkippedSameCommit,
        ApplyOutcome::SkippedConcurrency => FireOutcome::SkippedConcurrency,
        ApplyOutcome::SkippedByCommitMessage => {
            unreachable!("a manual fire does not honor the skip marker")
        }
    })
}

async fn stamp_last_fired(db: &gradient_db::WebDb, row: &MTaskTrigger) {
    let now = gradient_types::now();
    let mut active: gradient_entity::task_trigger::ActiveModel = row.clone().into();
    active.last_fired_at = Set(Some(now));
    active.updated_at = Set(now);
    if let Err(e) = gradient_db::triggers::update(db, active).await {
        tracing::warn!(error = %e, trigger_id = %row.id, "failed to stamp trigger last_fired_at");
    }
}
