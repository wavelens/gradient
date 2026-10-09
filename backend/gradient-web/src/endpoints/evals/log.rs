/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_project_member;
use crate::authorization::MaybeApiKey;
use crate::error::WebError;
use async_stream::stream;
use axum::Extension;
use axum::extract::{Path, State};
use axum_streams::StreamBodyAs;
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use gradient_util::log_lines::PrefixedLines;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;

use super::EvalAccessContext;

async fn eval_shared_build_jobs(
    state: &Arc<ServerState>,
    evaluation: EvaluationId,
) -> Result<Vec<(MDerivationBuild, String)>, WebError> {
    let jobs = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .all(&state.web_db)
        .await?;

    let shared_build_ids: Vec<DerivationBuildId> =
        jobs.iter().map(|j| j.derivation_build).collect();
    let shared_builds: HashMap<DerivationBuildId, MDerivationBuild> = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(shared_build_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|a| (a.id, a))
        .collect();

    let drv_ids: Vec<DerivationId> = jobs.iter().map(|j| j.derivation).collect();
    let names: HashMap<DerivationId, String> = EDerivation::find()
        .filter(CDerivation::Id.is_in(drv_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|d| (d.id, d.name))
        .collect();

    let mut out = Vec::with_capacity(jobs.len());
    for job in jobs {
        let Some(shared_build) = shared_builds.get(&job.derivation_build).cloned() else {
            continue;
        };
        let name = names.get(&job.derivation).cloned().unwrap_or_default();
        out.push((shared_build, name));
    }

    Ok(out)
}

struct Followed {
    offset: usize,
    lines: PrefixedLines,
    done: bool,
}

impl Followed {
    fn new(name: String) -> Self {
        Self {
            offset: 0,
            lines: PrefixedLines::new(name),
            done: false,
        }
    }
}

async fn read_new_lines(
    state: &ServerState,
    shared_build: DerivationBuildId,
    followed: &mut Followed,
    finished: bool,
) -> Vec<String> {
    let log = match gradient_db::scheduling::build_attempt::latest_attempt_id(
        &state.web_db,
        shared_build,
    )
    .await
    .unwrap_or(None)
    {
        Some(key) => state.log_storage.read(key).await.unwrap_or_default(),
        None => String::new(),
    };

    let mut lines = log
        .get(followed.offset..)
        .map(|new| followed.lines.push(new))
        .unwrap_or_default();
    followed.offset = log.len();
    if finished {
        lines.extend(followed.lines.finish());
        followed.done = true;
    }

    lines
}

pub async fn post_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> Result<StreamBodyAs<'static>, WebError> {
    let api_key_ref = api_key.as_ref();
    let ctx =
        EvalAccessContext::load(&state, evaluation_id, &Some(user.clone()), api_key_ref).await?;

    if !is_project_member(&state, user.id, ctx.project_id, api_key_ref).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    let evaluation = ctx.evaluation;

    let stream = stream! {
        let mut followed: HashMap<DerivationBuildId, Followed> = HashMap::new();
        let mut first = true;

        loop {
            let current = match eval_shared_build_jobs(&state, evaluation.id).await {
                Ok(jobs) => jobs,
                Err(e) => {
                    error!(error = %e, "Failed to query builds");
                    break;
                }
            };

            let mut any_pending = false;
            for (shared_build, name) in current {
                let building = shared_build.status == BuildStatus::Building;
                let finished = !matches!(
                    shared_build.status,
                    BuildStatus::Created | BuildStatus::Queued | BuildStatus::Building
                );
                any_pending |= matches!(shared_build.status, BuildStatus::Queued | BuildStatus::Building);
                let known = followed.contains_key(&shared_build.id);
                if !(first || building || (known && finished)) {
                    continue;
                }

                let entry = followed
                    .entry(shared_build.id)
                    .or_insert_with(|| Followed::new(name));
                if entry.done {
                    continue;
                }

                let lines = read_new_lines(&state, shared_build.id, entry, finished).await;
                if !lines.is_empty() {
                    yield lines.join("\n") + "\n";
                }
            }
            first = false;

            // The stream must stay open until the evaluation itself has finished. At the start it
            // is still evaluating and is carrying no build_job yet.
            if !any_pending {
                let still_active = EEvaluation::find_by_id(evaluation.id)
                    .one(&state.web_db)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|e| e.status.is_active());
                if !still_active {
                    yield "".to_string();
                    break;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    };

    Ok(StreamBodyAs::json_nl(
        crate::endpoints::log_stream::keepalive_chunks(stream),
    ))
}
