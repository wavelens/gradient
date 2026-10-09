/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_project_member;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;

use super::EvalAccessContext;
use super::types::MakeEvaluationRequest;

pub async fn post_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(evaluation_id): Path<EvaluationId>,
    Json(body): Json<MakeEvaluationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key_ref = api_key.as_ref();
    let ctx =
        EvalAccessContext::load(&state, evaluation_id, &Some(user.clone()), api_key_ref).await?;

    // Mutations require explicit project membership even on a public project.
    if !is_project_member(&state, user.id, ctx.project_id, api_key_ref).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    if body.method == "abort" {
        scheduler.abort_evaluation(ctx.evaluation).await;
    }

    Ok(ok_json("Success".to_string()))
}

pub async fn post_evaluation_retry(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key_ref = api_key.as_ref();
    let ctx =
        EvalAccessContext::load(&state, evaluation_id, &Some(user.clone()), api_key_ref).await?;
    if !is_project_member(&state, user.id, ctx.project_id, api_key_ref).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    if !EvaluationStatus::TERMINAL.contains(&ctx.evaluation.status) {
        return Err(WebError::conflict("The evaluation is still running"));
    }

    let task = match ctx.evaluation.task {
        Some(task) => ETask::find_by_id(task).one(&state.web_db).await?,
        None => None,
    }
    .ok_or_else(|| WebError::conflict("The evaluation belongs to no task"))?;

    let retried = gradient_ci::trigger_evaluation_retry(&state.web_db, &task, &ctx.evaluation)
        .await
        .map_err(|e| match e {
            gradient_ci::TriggerError::AlreadyInProgress => {
                WebError::conflict("Another evaluation of the task is running")
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
            .map_err(|e| WebError::internal(format!("the retry did not reach the graph: {e}")))?;
    }

    Ok(ok_json(retried.id.to_string()))
}

pub async fn post_evaluation_prioritize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_superuser(&user)?;
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Evaluation")?;

    scheduler
        .prioritize_evaluation(evaluation.id)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;

    Ok(ok_json("Success".to_string()))
}
