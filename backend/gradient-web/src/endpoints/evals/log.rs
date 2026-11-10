/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::can_view_project;
use crate::authorization::MaybeApiKey;
use crate::error::WebError;
use crate::resource::Resource;
use async_stream::stream;
use axum::Extension;
use axum::extract::{Path, State};
use axum_streams::StreamBodyAs;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::lookup::{by_id, by_ids};
use gradient_db::status::log_tail::LogTail;
use gradient_entity::build::BuildStatus;
use gradient_storage::LogStorage;
use gradient_types::*;
use gradient_util::log_lines::PrefixedLines;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, warn};

use super::EvalAccessContext;

async fn eval_shared_build_jobs(
    db: &WebDb,
    evaluation: EvaluationId,
) -> Result<Vec<(MDerivationBuild, String)>, WebError> {
    let jobs = gradient_db::build_jobs::of_evaluation(db, evaluation).await?;

    let shared_build_ids: Vec<DerivationBuildId> =
        jobs.iter().map(|j| j.derivation_build).collect();
    let shared_builds: HashMap<DerivationBuildId, MDerivationBuild> =
        by_ids::<EDerivationBuild, _>(db, &shared_build_ids).await?;

    let drv_ids: Vec<DerivationId> = jobs.iter().map(|j| j.derivation).collect();
    let names: HashMap<DerivationId, String> = by_ids::<EDerivation, _>(db, &drv_ids)
        .await?
        .into_iter()
        .map(|(id, d)| (id, d.name))
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
    tail: LogTail,
    lines: PrefixedLines,
    done: bool,
}

impl Followed {
    fn new(name: String) -> Self {
        Self {
            tail: LogTail::default(),
            lines: PrefixedLines::new(name),
            done: false,
        }
    }
}

async fn read_new_lines(
    db: &WebDb,
    log_storage: &Arc<dyn LogStorage>,
    shared_build: DerivationBuildId,
    followed: &mut Followed,
    finished: bool,
) -> anyhow::Result<Vec<String>> {
    let mut lines =
        match gradient_db::scheduling::build_attempt::latest_attempt_id(db, shared_build).await? {
            Some(attempt) => {
                let appended = followed
                    .tail
                    .appended(db, log_storage.as_ref(), attempt)
                    .await?;
                followed.lines.push(&appended)
            }
            None => Vec::new(),
        };
    if finished {
        lines.extend(followed.lines.finish());
        followed.done = true;
    }

    Ok(lines)
}

#[utoipa::path(
    post,
    path = "/evals/{evaluation}/builds",
    operation_id = "streamEvaluationLogs",
    tag = "evals",
    summary = "Stream evaluation logs",
    description = "Open a streaming NDJSON response: replay the stored logs of all builds\nfirst, then follow the derivations still building. A derivation-name\nprefix opens each line. Keep the stream open until the evaluation itself\nhas reached Completed, Failed or Aborted, not merely until no build is\npending, so an evaluation without builds yet still has its stream.\nEvaluation-level messages are not part of this stream. Fetch them from\n`GET /evals/{evaluation}/messages`.\n\nChunks hold no more than 256 KiB each, split at a line end. Empty strings\narrive as keepalives after 15 seconds without output. Ignore them.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "NDJSON log stream", body = String, content_type = "application/x-ndjson"), WebError)
)]
pub async fn post_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> Result<StreamBodyAs<'static>, WebError> {
    let api_key_ref = api_key.as_ref();
    let ctx = EvalAccessContext::load(
        &state.web_db,
        evaluation_id,
        &Some(user.clone()),
        api_key_ref,
    )
    .await?;

    if !can_view_project(&state.web_db, user.id, ctx.project_id, api_key_ref).await? {
        return Err(WebError::not_found(Resource::Evaluation));
    }

    let evaluation = ctx.evaluation;

    let stream = stream! {
        let mut followed: HashMap<DerivationBuildId, Followed> = HashMap::new();
        let mut first = true;

        'poll_evaluation_builds: loop {
            let current = match eval_shared_build_jobs(&state.web_db, evaluation.id).await {
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
                    if finished {
                        continue;
                    }
                    entry.done = false;
                }

                match read_new_lines(&state.web_db, &state.log_storage, shared_build.id, entry, finished).await {
                    Ok(lines) if lines.is_empty() => {}
                    Ok(lines) => yield lines.join("\n") + "\n",
                    Err(e) => {
                        warn!(shared_build_id = %shared_build.id, error = %e, "log stream for evaluation failed to read the build log");
                        break 'poll_evaluation_builds;
                    }
                }
            }
            first = false;

            // The stream must stay open until the evaluation itself has finished. At the start it
            // is still evaluating and is carrying no build_job yet.
            if !any_pending {
                let still_active = by_id::<EEvaluation, _>(&state.web_db, evaluation.id).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build_attempt;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::log_storage::{InMemoryLogStorage, ReadErrorLogStorage};

    fn attempt_row(id: BuildAttemptId, shared_build: DerivationBuildId) -> build_attempt::Model {
        build_attempt::Model {
            id,
            derivation_build: shared_build,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn read_new_lines_error_leaves_the_build_unfinished() {
        let shared_build = DerivationBuildId::now_v7();
        let db = WebDb::new(
            mock_db()
                .append_query_results([vec![attempt_row(BuildAttemptId::now_v7(), shared_build)]])
                .into_connection(),
        );
        let log_storage: Arc<dyn LogStorage> = Arc::new(ReadErrorLogStorage);
        let mut followed = Followed::new("hello".into());

        let result = read_new_lines(&db, &log_storage, shared_build, &mut followed, true).await;

        assert!(result.is_err());
        assert!(!followed.done);
    }

    #[tokio::test]
    async fn a_retried_build_streams_the_new_attempt_from_its_start() {
        let shared_build = DerivationBuildId::now_v7();
        let (first, retry) = (BuildAttemptId::now_v7(), BuildAttemptId::now_v7());
        let db = WebDb::new(
            mock_db()
                .append_query_results([vec![attempt_row(first, shared_build)]])
                .append_query_results([vec![attempt_row(retry, shared_build)]])
                .into_connection(),
        );
        let storage = InMemoryLogStorage::new();
        storage.seed(first, "a long first attempt\n");
        storage.seed(retry, "retry\n");
        let log_storage: Arc<dyn LogStorage> = Arc::new(storage);
        let mut followed = Followed::new("pkg".into());

        let first_lines = read_new_lines(&db, &log_storage, shared_build, &mut followed, false)
            .await
            .unwrap();
        let retry_lines = read_new_lines(&db, &log_storage, shared_build, &mut followed, false)
            .await
            .unwrap();

        assert_eq!(first_lines.len(), 1);
        assert!(first_lines[0].ends_with("a long first attempt"));
        assert_eq!(retry_lines.len(), 1);
        assert!(retry_lines[0].ends_with("retry"));
    }
}
