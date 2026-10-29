/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::exec_jobs::pull_requests;
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

use super::ExecJobAccessContext;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobPullRequestView {
    pub url: String,
    pub branch: String,
    pub number: i64,
    pub integration: String,
    pub created_at: NaiveDateTime,
}

#[utoipa::path(
    get,
    path = "/exec/{job}/pull-requests",
    operation_id = "listExecJobPullRequests",
    tag = "exec-jobs",
    summary = "List pull requests of an exec job",
    description = "Returns the pull requests the job opened or updated, newest first. `integration` is the id of the Git host integration that opened it.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Pull requests", body = Vec<ExecJobPullRequestView>), WebError)
)]
pub async fn list_exec_job_pull_requests(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<Vec<ExecJobPullRequestView>>>> {
    ExecJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    let rows = pull_requests::of_job(&state.web_db, job).await?;
    Ok(ok_json(
        rows.into_iter()
            .map(|row| ExecJobPullRequestView {
                url: row.url,
                branch: row.branch,
                number: row.number,
                integration: row.integration.to_string(),
                created_at: row.created_at,
            })
            .collect(),
    ))
}
