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
use gradient_core::ServerState;
use gradient_db::scheduling::build_attempt::list_build_attempts_with_worker_id_newest_first;
use gradient_types::*;
use std::sync::Arc;

use super::BuildAccessContext;

#[utoipa::path(
    get,
    path = "/builds/{build}/attempts",
    operation_id = "getBuildAttempts",
    tag = "builds",
    summary = "List of Build Attempts",
    description = "All attempts of a build appear newest first. Retries of a build open a new attempt. Logs of earlier attempts remain. Pass the `id` of an attempt as the `attempt` query parameter to the endpoints for the build log to read the log of that attempt.\n\n**Access:** Same as `GET /builds/{build}/log`.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Build attempts, newest first", body = Vec<BuildAttemptLogRow>), WebError)
)]
pub async fn get_build_attempts(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<Vec<BuildAttemptLogRow>>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let attempts =
        list_build_attempts_with_worker_id_newest_first(&state.web_db, ctx.shared_build.id)
            .await?
            .into_iter()
            .map(|(attempt, worker)| BuildAttemptLogRow::new(attempt, worker))
            .collect();

    Ok(ok_json(attempts))
}
