/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DeletedResponse;
use super::refusal::TaskRefusal;
use crate::api_spec::{ProjectPath, TaskPath};
use crate::audit::Audit;
use crate::endpoints::git_host_connections::check_task_repository_url_can_use_git_host_connection;
use crate::error::{WebError, WebResult};
use crate::extractors::JsonBody;
use crate::extractors::Query;
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Requires, RequiresUnmanaged, TaskScope, can};
use anyhow::Context;
use axum::{Extension, Json};
use chrono::Utc;
use gradient_ci::actions::{encrypt_secret_with_file, new_webhook_secret};
use gradient_ci::trigger::InputUpdateOutcome;
use gradient_ci::{ApplyInput, ApplyOutcome, apply_trigger};
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_entity::evaluation::WalkMode;
use gradient_scheduler::Scheduler;
use gradient_sources::head_commit;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
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
        .routes(routes!(rotate_trigger_secret))
}

#[derive(Serialize, ToSchema)]
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
    pub has_secret: bool,
    pub post_request_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_secret: Option<String>,
}

fn trigger_public_post_url(serve_url: &str, trigger: &MTaskTrigger) -> Option<String> {
    (trigger.trigger_type == TriggerType::PostRequest).then(|| {
        format!(
            "{}/api/v1/hooks/trigger/{}",
            serve_url.trim_end_matches('/'),
            trigger.id
        )
    })
}

fn trigger_api_response_from_row_and_base_url(row: MTaskTrigger, serve_url: &str) -> TriggerOut {
    TriggerOut {
        post_request_url: trigger_public_post_url(serve_url, &row),
        has_secret: row.encrypted_secret.is_some(),
        generated_secret: None,
        id: row.id,
        task: row.task,
        trigger_type: row.trigger_type,
        config: row.config,
        active: row.active,
        is_default: row.is_default,
        last_fired_at: row.last_fired_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn create_trigger_secret_with_encrypted_copy(crypt_file: &str) -> WebResult<(String, String)> {
    let plaintext = new_webhook_secret();
    let ciphertext = encrypt_secret_with_file(crypt_file, &plaintext)
        .context("encrypt the task trigger secret")?;
    Ok((plaintext, ciphertext))
}

fn encrypted_secret_change_for_trigger_type_update(
    active: &mut ATaskTrigger,
    trigger_type: TriggerType,
    crypt_file: &str,
) -> WebResult<Option<String>> {
    if trigger_type != TriggerType::PostRequest {
        active.encrypted_secret = Set(None);
        return Ok(None);
    }
    if active
        .encrypted_secret
        .try_as_ref()
        .is_some_and(Option::is_some)
    {
        return Ok(None);
    }
    let (plaintext, ciphertext) = create_trigger_secret_with_encrypted_copy(crypt_file)?;
    active.encrypted_secret = Set(Some(ciphertext));
    Ok(Some(plaintext))
}

fn build_trigger_audit_log_json(trigger: &MTaskTrigger) -> serde_json::Value {
    serde_json::json!({
        "trigger_id": trigger.id.to_string(),
        "trigger_type": trigger.trigger_type,
        "active": trigger.active,
    })
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

fn default_push_and_pull_request_trigger_suggestions() -> Vec<TriggerConfig> {
    vec![
        TriggerConfig::ReporterPush {
            branches: vec![],
            tags: vec![],
            releases_only: false,
            include_flake_inputs: false,
            path_filter: ChangedFilePathFilter::default(),
        },
        TriggerConfig::ReporterPullRequest {
            branches: vec![],
            actions: ["opened", "synchronize", "reopened"]
                .map(String::from)
                .to_vec(),
            require_approval: true,
            path_filter: ChangedFilePathFilter::default(),
        },
    ]
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
    description = "List the triggers for pushes and pull requests that a task with `repository` can add. Entries appear in the list when the project can use a connection to a Git host that can receive events and match the host of the repository. Otherwise the list is empty. Gradient doesn't store suggestions. Use the endpoint to create a task or the endpoint to create a trigger.",
    params(ProjectPath, TriggerSuggestionsQuery),
    responses((status = OK, description = "Recommended Triggers", body = Vec<TriggerConfig>), WebError)
)]
pub async fn get_project_repository_trigger_suggestions(
    ProjectScope { project, db, .. }: ProjectScope<Member>,
    Query(query): Query<TriggerSuggestionsQuery>,
) -> WebResult<Json<BaseResponse<Vec<TriggerConfig>>>> {
    let connections =
        gradient_db::git_host_connections::list_git_host_connections_available_to_project(
            &db.web_db, &project,
        )
        .await?;
    let served = connections.iter().any(|connection| {
        connection.receives_events
            && check_task_repository_url_can_use_git_host_connection(&query.repository, connection)
                .is_ok()
    });
    Ok(ok_json(if served {
        default_push_and_pull_request_trigger_suggestions()
    } else {
        Vec::new()
    }))
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
    let serve_url = &db.config.server.serve_url;

    Ok(ok_json(
        rows.into_iter()
            .map(|row| trigger_api_response_from_row_and_base_url(row, serve_url))
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
    TaskScope { task: proj, db, .. }: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
    JsonBody(body): JsonBody<CreateTriggerRequest>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let config = validate_trigger_config_and_return_config_to_store(body.config)?;
    let mut trigger = build_task_trigger_row_from_config_and_flags(
        proj.id,
        &config,
        body.active,
        false,
        gradient_types::now(),
    );
    let generated_secret = (trigger.trigger_type == TriggerType::PostRequest)
        .then(|| create_trigger_secret_with_encrypted_copy(&db.config.secrets.crypt_file))
        .transpose()?
        .map(|(plaintext, ciphertext)| {
            trigger.encrypted_secret = Some(ciphertext);
            plaintext
        });
    let row = gradient_db::triggers::insert(&db.web_db, trigger).await?;

    audit
        .write(
            Action::TaskEvaluationTriggerCreate,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&row),
        )
        .await;
    Ok(ok_json(TriggerOut {
        generated_secret,
        ..trigger_api_response_from_row_and_base_url(row, &db.config.server.serve_url)
    }))
}

pub(crate) fn validate_trigger_config_and_return_config_to_store(
    config: TriggerConfig,
) -> WebResult<TriggerConfig> {
    config
        .validate_path_filter_pattern_limits()
        .and_then(|()| config.validate_and_convert_cron_to_six_fields())
        .map_err(|e| TaskRefusal::InvalidTriggerConfig(e.to_string()).into())
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

    Ok(ok_json(trigger_api_response_from_row_and_base_url(
        row,
        &db.config.server.serve_url,
    )))
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
        task: proj,
        db,
        params,
        ..
    }: TaskScope<RequiresUnmanaged<can::ManageTriggers>>,
    audit: Audit,
    JsonBody(body): JsonBody<UpdateTriggerRequest>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = gradient_db::triggers::by_id_in_task(&db.web_db, proj.id, id)
        .await?
        .or_not_found(Resource::Trigger)?;

    let config = match body.config {
        Some(cfg) => Some(validate_trigger_config_and_return_config_to_store(cfg)?),
        None => None,
    };

    let mut active: ATaskTrigger = row.into();
    let mut generated_secret = None;
    if let Some(cfg) = config {
        let tt = cfg.trigger_type();
        generated_secret = encrypted_secret_change_for_trigger_type_update(
            &mut active,
            tt,
            &db.config.secrets.crypt_file,
        )?;
        active.trigger_type = Set(tt);
        active.config = Set(cfg.to_db_json());
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = gradient_db::triggers::update(&db.web_db, active).await?;

    audit
        .write(
            Action::TaskEvaluationTriggerChanged,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&updated),
        )
        .await;
    Ok(ok_json(TriggerOut {
        generated_secret,
        ..trigger_api_response_from_row_and_base_url(updated, &db.config.server.serve_url)
    }))
}

#[utoipa::path(
    post,
    path = "/{id}/secret",
    operation_id = "rotateTaskTriggerSecret",
    tag = "triggers",
    summary = "Replace trigger secret",
    description = "Generate a new secret for the `post_request` trigger, encrypt it, and store it. Return the new secret once in `generated_secret` in the response. Old secrets stop working immediately. Reject other trigger types.",
    params(TaskPath, ("id" = TaskTriggerId, Path)),
    responses((status = OK, description = "Trigger with the new secret", body = TriggerOut), WebError)
)]
pub async fn rotate_trigger_secret(
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
    if row.trigger_type != TriggerType::PostRequest {
        return Err(TaskRefusal::TriggerSecretReplacementNeedsPostRequestTrigger.into());
    }

    let (plaintext, ciphertext) =
        create_trigger_secret_with_encrypted_copy(&db.config.secrets.crypt_file)?;
    let mut active: ATaskTrigger = row.into();
    active.encrypted_secret = Set(Some(ciphertext));
    active.updated_at = Set(gradient_types::now());
    let updated = gradient_db::triggers::update(&db.web_db, active).await?;

    audit
        .write(
            Action::TaskEvaluationTriggerChanged,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&updated),
        )
        .await;
    Ok(ok_json(TriggerOut {
        generated_secret: Some(plaintext),
        ..trigger_api_response_from_row_and_base_url(updated, &db.config.server.serve_url)
    }))
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

    audit
        .write(
            Action::TriggerBecameDefaultForTask,
            EventOwner::task(proj.project, proj.id),
            build_trigger_audit_log_json(&updated),
        )
        .await;
    Ok(ok_json(trigger_api_response_from_row_and_base_url(
        updated,
        &db.config.server.serve_url,
    )))
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

    let resolved = head_commit(db, task, branch.as_deref()).await;
    let fetch_error = resolved.as_ref().err().map(|e| e.short_reason_for_user());
    if let Err(e) = gradient_db::tasks::record_fetch(
        &db.web_db,
        task.id,
        gradient_types::now(),
        fetch_error.as_deref(),
    )
    .await
    {
        tracing::warn!(task_id = %task.id, error = %e, "Failed to record the repository fetch result");
    }
    let (branch, head) = resolved.map_err(|e| {
        tracing::warn!(task_id = %task.id, trigger_id = %trigger.id, error = %e, "rejecting evaluation: failed to fetch the head commit of the repository");
        SharedRefusal::repository_state_fetch_failed_from_git_source_error(&e)
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
                code_trust: gradient_ci::EvaluationCodeTrust::CodeWithProjectCredentials,
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
            aborted_evaluations,
        } => {
            for (aborted_id, hard_abort) in aborted_evaluations {
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
