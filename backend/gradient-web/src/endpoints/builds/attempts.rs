/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::scheduling::build_attempt::load_build_attempts_with_worker_ids;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::BuildAccessContext;

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct BuildAttemptResponse {
    pub number: u32,
    pub outcome: AttemptOutcome,
    pub reason: Option<AttemptFailureReason>,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub worker: Option<String>,
}

#[utoipa::path(
    get,
    path = "/builds/{build}/attempts",
    operation_id = "listBuildAttempts",
    tag = "builds",
    summary = "List build attempts",
    description = "Returns every attempt of the build, oldest first. `number` counts from 1 and is the value that the `attempt` parameter of the log endpoints takes. A build is shared between evaluations of the same derivation, so the list can contain attempts that another evaluation started. `worker` is the ID of the worker that ran the attempt. It is empty for an attempt that failed before a worker picked it up.\n\n**Access:** is identical to `GET /builds/{build}/log`.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Build Attempts", body = Vec<BuildAttemptResponse>), crate::error::WebError)
)]
pub async fn list_build_attempts(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<Vec<BuildAttemptResponse>>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let attempts = load_build_attempts_with_worker_ids(&state.web_db, ctx.shared_build.id).await?;

    Ok(ok_json(
        attempts
            .into_iter()
            .zip(1u32..)
            .map(|((attempt, worker), number)| BuildAttemptResponse {
                number,
                outcome: attempt.outcome,
                reason: attempt.reason,
                started_at: attempt.build_started_at,
                finished_at: attempt.build_finished_at,
                worker,
            })
            .collect(),
    ))
}
