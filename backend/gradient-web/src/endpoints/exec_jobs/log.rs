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
use gradient_db::exec_jobs::log_key;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::ExecJobAccessContext;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExecJobLogQuery {
    pub attempt: Option<i32>,
    pub node: Option<u32>,
    pub offset: Option<usize>,
}

#[derive(Serialize, Debug, PartialEq, Eq, utoipa::ToSchema)]
pub struct ExecJobLog {
    pub offset: usize,
    pub log: String,
    pub total_bytes: usize,
}

impl ExecJobLog {
    fn after(mut full: String, offset: Option<usize>) -> Self {
        let total_bytes = full.len();
        let offset = offset
            .filter(|&offset| full.is_char_boundary(offset))
            .unwrap_or(0);
        let log = full.split_off(offset);
        Self {
            offset,
            log,
            total_bytes,
        }
    }
}

#[utoipa::path(
    get,
    path = "/exec/{job}/log",
    operation_id = "getExecJobLog",
    tag = "exec-jobs",
    summary = "Get exec job log",
    description = "Returns the program output of one attempt, the latest attempt unless `attempt` names its number. `node` picks the node of a multi-node job by its index, node 0 by default. `offset` is the number of bytes the caller already holds: the answer then carries only the bytes after it and `offset` in the answer says where `log` starts (0 when the requested offset is past the end or inside a character, so the caller replaces its copy). `total_bytes` is the full log size to pass as the next `offset`. Empty log before the first attempt. Answers 404 for an attempt number the node does not have and for a node index the job does not have.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = ExecJobId, Path), ExecJobLogQuery),
    responses((status = OK, description = "Exec job log", body = ExecJobLog), WebError)
)]
pub async fn get_exec_job_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<ExecJobId>,
    Query(query): Query<ExecJobLogQuery>,
) -> WebResult<Json<BaseResponse<ExecJobLog>>> {
    let ctx = ExecJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    let attempts = &ctx.node(query.node)?.attempts;
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
    Ok(ok_json(ExecJobLog::after(log, query.offset)))
}

#[cfg(test)]
mod tests {
    use super::ExecJobLog;

    fn after(full: &str, offset: Option<usize>) -> (usize, String, usize) {
        let chunk = ExecJobLog::after(full.to_owned(), offset);
        (chunk.offset, chunk.log, chunk.total_bytes)
    }

    #[test]
    fn offset_returns_only_the_new_bytes() {
        assert_eq!(after("abcdef", Some(4)), (4, "ef".into(), 6));
        assert_eq!(after("abcdef", Some(6)), (6, String::new(), 6));
    }

    #[test]
    fn missing_or_unusable_offset_returns_the_whole_log() {
        assert_eq!(after("abc", None), (0, "abc".into(), 3));
        assert_eq!(after("abc", Some(9)), (0, "abc".into(), 3));
        assert_eq!(after("a×b", Some(2)), (0, "a×b".into(), 4));
    }
}
