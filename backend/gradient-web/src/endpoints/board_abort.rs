/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorCode, WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::lookup::require;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::scheduling::assignment_record::shared_build_of_key;
use gradient_entity::dispatched_job::{self, DispatchedJobKind};
use gradient_scheduler::Scheduler;
use gradient_types::ids::DispatchedJobId;
use gradient_types::*;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
enum JobAbort {
    Evaluation,
    Build(DerivationBuildId),
    BuildForAllEvaluations(DerivationBuildId),
    Worker(String),
}

fn job_abort(job: &dispatched_job::Model, evaluation_running: bool) -> Option<JobAbort> {
    let job_id = job.job_id.clone()?;
    let shared_build = shared_build_of_key(&job_id);
    match (job.kind, shared_build) {
        (DispatchedJobKind::Exec, _) | (DispatchedJobKind::Build, None) => None,
        (DispatchedJobKind::Build, Some(shared_build)) if evaluation_running => {
            Some(JobAbort::Build(shared_build))
        }
        (DispatchedJobKind::Build, Some(shared_build)) => {
            Some(JobAbort::BuildForAllEvaluations(shared_build))
        }
        (DispatchedJobKind::Eval, _) if evaluation_running => Some(JobAbort::Evaluation),
        (DispatchedJobKind::Eval, _) => Some(JobAbort::Worker(job_id)),
    }
}

fn not_abortable(reason: &str) -> WebError {
    WebError::Conflict(ErrorCode::CONFLICT, reason.to_owned())
}

#[utoipa::path(
    post,
    path = "/board/jobs/{id}/abort",
    operation_id = "abortBoardJob",
    tag = "board",
    summary = "Abort assigned job",
    description = "Stops a running build or eval job. Under a running evaluation, an eval job aborts the evaluation and a build job aborts the build as `POST /builds/{build}/abort` does. Without one, a build job aborts the build for every finished evaluation that still wants it and stays aborted and an eval job is stopped on its worker. Superuser-only. Answers 409 when the job has finished, is an exec job, or another evaluation still needs the build.",
    params(("id" = Uuid, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_board_job_abort(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_superuser(&user)?;

    let job = require::<dispatched_job::Entity>(&state.web_db, DispatchedJobId::from(id)).await?;
    if job.finished_at.is_some() {
        return Err(not_abortable("The job has already finished"));
    }

    let evaluation = require::<EEvaluation>(&state.web_db, job.evaluation_id).await?;
    let running = !evaluation.status.is_terminal();
    match job_abort(&job, running) {
        Some(JobAbort::Evaluation) => scheduler.abort_evaluation(evaluation).await,
        Some(JobAbort::Build(shared_build)) => {
            scheduler.abort_build(evaluation.id, shared_build).await??
        }
        Some(JobAbort::BuildForAllEvaluations(shared_build)) => {
            scheduler
                .abort_build_for_all_evaluations(evaluation.id, shared_build)
                .await??
        }
        Some(JobAbort::Worker(job_id)) => scheduler.stop_job(job_id).await?,
        None => return Err(not_abortable("Abort an exec job on its own page")),
    }

    Ok(ok_json("Success".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: DispatchedJobKind, job_id: &str) -> dispatched_job::Model {
        dispatched_job::Model {
            kind,
            job_id: Some(job_id.to_owned()),
            ..Default::default()
        }
    }

    fn build_key(shared_build: DerivationBuildId) -> String {
        format!("build:{shared_build}")
    }

    #[test]
    fn a_build_job_without_a_running_evaluation_aborts_the_build_for_all_evaluations() {
        let shared_build = DerivationBuildId::now_v7();
        assert_eq!(
            job_abort(
                &job(DispatchedJobKind::Build, &build_key(shared_build)),
                false
            ),
            Some(JobAbort::BuildForAllEvaluations(shared_build))
        );
    }

    #[test]
    fn an_eval_job_without_a_running_evaluation_is_stopped_on_its_worker() {
        let key = format!("eval:{}", EvaluationId::now_v7());
        assert_eq!(
            job_abort(&job(DispatchedJobKind::Eval, &key), false),
            Some(JobAbort::Worker(key))
        );
    }

    #[test]
    fn a_build_job_of_a_running_evaluation_aborts_the_build() {
        let shared_build = DerivationBuildId::now_v7();
        assert_eq!(
            job_abort(
                &job(DispatchedJobKind::Build, &build_key(shared_build)),
                true
            ),
            Some(JobAbort::Build(shared_build))
        );
    }

    #[test]
    fn an_eval_job_of_a_running_evaluation_aborts_the_evaluation() {
        let key = format!("eval:{}", EvaluationId::now_v7());
        assert_eq!(
            job_abort(&job(DispatchedJobKind::Eval, &key), true),
            Some(JobAbort::Evaluation)
        );
    }

    #[test]
    fn an_exec_job_is_refused() {
        let key = format!("exec:{}", ExecJobId::now_v7());
        assert_eq!(job_abort(&job(DispatchedJobKind::Exec, &key), false), None);
    }
}
