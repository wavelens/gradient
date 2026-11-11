/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::EvaluationRefusal;
use crate::access::require_project_permission;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::lookup::require;
use crate::permissions::Permission;
use crate::resource::Resource;
use anyhow::Context;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::lookup::by_id;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use std::sync::Arc;

use super::EvalAccessContext;
use super::types::{EvaluationAction, MakeEvaluationRequest};

async fn load_abortable_evaluation(
    db: &gradient_db::WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    evaluation_id: EvaluationId,
) -> WebResult<EvalAccessContext> {
    if user.superuser {
        return EvalAccessContext::load_unguarded(db, evaluation_id).await;
    }

    load_triggerable_evaluation(db, user, api_key, evaluation_id).await
}

async fn load_triggerable_evaluation(
    db: &gradient_db::WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    evaluation_id: EvaluationId,
) -> WebResult<EvalAccessContext> {
    let api_key_ref = api_key.as_ref();
    let ctx = EvalAccessContext::load(db, evaluation_id, &Some(user.clone()), api_key_ref).await?;
    require_project_permission(
        db,
        user.id,
        ctx.project_id,
        Permission::TriggerEvaluation,
        Resource::Evaluation,
        api_key_ref,
    )
    .await?;
    Ok(ctx)
}

#[utoipa::path(
    post,
    path = "/evals/{evaluation}",
    operation_id = "postEvaluation",
    tag = "evals",
    summary = "Evaluation action",
    description = "Performs an action on the evaluation. Currently the only supported action is\n`\"abort\"`, which cancels all in-progress and queued builds. Callers need the `triggerEvaluation` permission on the project, or superuser access.",
    params(("evaluation" = EvaluationId, Path)),
    request_body = MakeEvaluationRequest,
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(evaluation_id): Path<EvaluationId>,
    Json(body): Json<MakeEvaluationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_abortable_evaluation(&state.web_db, &user, &api_key, evaluation_id).await?;

    match body.method {
        EvaluationAction::Abort => {
            scheduler
                .abort_evaluation(ctx.evaluation, Some(user.id))
                .await
        }
    }

    Ok(ok_json("Success".to_string()))
}

#[utoipa::path(
    post,
    path = "/evals/{evaluation}/retry",
    operation_id = "retryEvaluation",
    tag = "evals",
    summary = "Retry evaluation",
    description = "Start a new evaluation for the same commit without another fetch or evaluation. Put the UUID in `message`. Completed evaluations stay immutable. Status, duration and logs stay intact. Set `retry_of` to the completed evaluation.\n\nThe new evaluation can requeue every build from the completed evaluation that failed, timed out, was canceled, or failed because of dependencies. Reproducible builder failures go back in the queue too, regardless of the `retry_failed_builds` value on the task. Reuse every build that already succeeded.\n\nTriggers don't run. Store the caller as `triggered_by`. The caller must have `triggerEvaluation` permission on the project. Return 409 during an active run of the evaluation or of another evaluation on its task.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "UUID of the new evaluation", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_evaluation_retry(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_triggerable_evaluation(&state.web_db, &user, &api_key, evaluation_id).await?;

    if !EvaluationStatus::TERMINAL.contains(&ctx.evaluation.status) {
        return Err(EvaluationRefusal::StillRunning.into());
    }

    let task = match ctx.evaluation.task {
        Some(task) => by_id::<ETask, _>(&state.web_db, task).await?,
        None => None,
    }
    .ok_or(EvaluationRefusal::WithoutTask)?;

    let retried =
        gradient_ci::trigger_evaluation_retry(&state.web_db, &task, &ctx.evaluation, user.id)
            .await
            .map_err(|e| match e {
                gradient_ci::TriggerError::AlreadyInProgress => {
                    WebError::from(EvaluationRefusal::TaskAlreadyEvaluating)
                }
                gradient_ci::TriggerError::Db(db_err) => WebError::from(db_err),
            })?;
    if retried.status == EvaluationStatus::Building {
        state
            .graph
            .transition(gradient_graph::Transition::Repair {
                scope: gradient_db::graph::repair::RepairScope::Retry(retried.id),
            })
            .await
            .context("the retry did not reach the graph")?;
    }

    Ok(ok_json(retried.id.to_string()))
}

#[utoipa::path(
    post,
    path = "/evals/{evaluation}/prioritize",
    operation_id = "prioritizeEvaluation",
    tag = "evals",
    summary = "Prioritize evaluation",
    description = "Hands out every build of an active evaluation, including builds it has not resolved yet, ahead of unprioritized work. Requires a superuser (`403` with `code: \"superuser_required\"` otherwise). A finished evaluation is left unchanged; a failed or aborted one loses its priority.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_evaluation_prioritize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_superuser(&user)?;
    let evaluation = require::<EEvaluation>(&state.web_db, evaluation_id).await?;

    scheduler
        .prioritize_evaluation(evaluation.id)
        .await
        .context("prioritize the evaluation")?;

    Ok(ok_json("Success".to_string()))
}
