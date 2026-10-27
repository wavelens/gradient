/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::runner_jobs::log_key;
use gradient_types::*;
use serde::Deserialize;
use std::sync::Arc;

use super::RunnerJobAccessContext;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RunnerJobLogQuery {
    pub attempt: Option<i32>,
}

#[utoipa::path(
    get,
    path = "/runner-jobs/{job}/log",
    operation_id = "getRunnerJobLog",
    tag = "runner-jobs",
    summary = "Get runner job log",
    description = "Returns the program output of one attempt as a single string, the latest attempt unless `attempt` names its number. Empty string before the first attempt. Answers 404 for an attempt number the job does not have.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = RunnerJobId, Path), RunnerJobLogQuery),
    responses((status = OK, description = "Runner job log", body = String, content_type = "application/json"), WebError)
)]
pub async fn get_runner_job_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<RunnerJobId>,
    Query(query): Query<RunnerJobLogQuery>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx =
        RunnerJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    let attempts = &ctx.detail.attempts;
    let attempt = match query.attempt {
        Some(number) => Some(
            attempts
                .iter()
                .find(|a| a.number == number)
                .ok_or_else(|| WebError::not_found(Resource::Log))?,
        ),
        None => attempts.last(),
    };
    let log = match attempt {
        Some(attempt) => state
            .log_storage
            .read(log_key(attempt.id))
            .await
            .unwrap_or_default(),
        None => String::new(),
    };
    Ok(ok_json(log))
}
