/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::EvaluationRefusal;
use crate::access::is_project_member;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::lookup::require;
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
    let ctx = EvalAccessContext::load(
        &state.web_db,
        evaluation_id,
        &Some(user.clone()),
        api_key_ref,
    )
    .await?;

    // Mutations require explicit project membership even on a public project.
    if !is_project_member(&state.web_db, user.id, ctx.project_id, api_key_ref).await? {
        return Err(WebError::not_found(Resource::Evaluation));
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
    let ctx = EvalAccessContext::load(
        &state.web_db,
        evaluation_id,
        &Some(user.clone()),
        api_key_ref,
    )
    .await?;
    if !is_project_member(&state.web_db, user.id, ctx.project_id, api_key_ref).await? {
        return Err(WebError::not_found(Resource::Evaluation));
    }

    if !EvaluationStatus::TERMINAL.contains(&ctx.evaluation.status) {
        return Err(EvaluationRefusal::StillRunning.into());
    }

    let task = match ctx.evaluation.task {
        Some(task) => by_id::<ETask, _>(&state.web_db, task).await?,
        None => None,
    }
    .ok_or(EvaluationRefusal::WithoutTask)?;

    let retried = gradient_ci::trigger_evaluation_retry(&state.web_db, &task, &ctx.evaluation)
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
