/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod closure;
pub mod downloads;
pub mod graph;
pub mod log;
pub mod log_chunks;
pub mod query;
mod refusal;
pub mod sbom;

pub use self::actions::{post_build_abort, post_build_prioritize, post_build_retry};
pub use self::closure::{
    ClosureEdge, ClosureGraph, ClosureNode, build_closure_graph, build_runtime_closure_graph,
    derivation_closure_reachable, get_build_closure, get_build_runtime_closure, get_eval_closure,
    get_eval_runtime_closure, sum_output_sizes,
};
pub use self::downloads::{
    BuildProduct, DownloadQuery, get_build_download, get_build_download_token, get_build_downloads,
};
pub use self::graph::{BuildGraph, DependencyEdge, get_build_graph};
pub use self::log::{get_build_log, post_build_log};
pub use self::log_chunks::{
    get_build_log_chunk, get_build_log_chunks, get_build_log_lines, get_build_log_search,
};
pub use self::query::{BuildWithOutputs, get_build};

use crate::access::is_project_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::lookup::require;
use crate::resource::Resource;
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_db::scheduling::build_attempt::latest_attempt_id;
use gradient_types::*;

pub(super) struct BuildAccessContext {
    pub build_job: MBuildJob,
    pub shared_build: MDerivationBuild,
    pub project: MProject,
}

impl BuildAccessContext {
    pub(super) async fn load_unguarded(db: &WebDb, build_job_id: BuildJobId) -> WebResult<Self> {
        let build_job = require::<EBuildJob>(db, build_job_id).await?;

        let shared_build = by_id::<EDerivationBuild, _>(db, build_job.derivation_build)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    shared_build_id = %build_job.derivation_build,
                    build_job_id = %build_job_id,
                    "shared build not found for build_job",
                );
                WebError::data_inconsistency(Resource::Build)
            })?;

        let evaluation = by_id::<EEvaluation, _>(db, build_job.evaluation)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    evaluation_id = %build_job.evaluation,
                    build_job_id = %build_job_id,
                    "Evaluation not found for build_job",
                );
                WebError::data_inconsistency(Resource::Build)
            })?;

        let task_id = evaluation.task.ok_or_else(|| {
            tracing::warn!(evaluation_id = %evaluation.id, "evaluation has no task");
            WebError::data_inconsistency(Resource::Evaluation)
        })?;

        let project_id = by_id::<ETask, _>(db, task_id)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    %task_id,
                    evaluation_id = %evaluation.id,
                    "Task not found for evaluation",
                );
                WebError::data_inconsistency(Resource::Evaluation)
            })?
            .project;

        let project = by_id::<EProject, _>(db, project_id).await?.ok_or_else(|| {
            tracing::warn!(%project_id, "Project not found");
            WebError::data_inconsistency(Resource::Project)
        })?;

        Ok(Self {
            build_job,
            shared_build,
            project,
        })
    }

    pub(super) async fn load(
        db: &WebDb,
        build_job_id: BuildJobId,
        maybe_user: &Option<MUser>,
        api_key: Option<&ApiKeyContext>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(db, build_job_id).await?;

        let direct_access = if ctx.project.public {
            true
        } else {
            match maybe_user {
                Some(user) => is_project_member(db, user.id, ctx.project.id, api_key).await?,
                None => false,
            }
        };

        if direct_access {
            return Ok(ctx);
        }

        if let Some(user) = maybe_user
            && reachable_projects_accessible(db, user, api_key, ctx.build_job.derivation).await?
        {
            return Ok(ctx);
        }

        Err(WebError::not_found(Resource::Build))
    }
}

async fn reachable_projects_accessible(
    db: &WebDb,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
    derivation: DerivationId,
) -> WebResult<bool> {
    let jobs = gradient_db::graph::reachability::build_jobs_for_derivation(db, derivation).await?;
    if jobs.is_empty() {
        return Ok(false);
    }

    let eval_ids: Vec<EvaluationId> = jobs.into_iter().map(|j| j.evaluation).collect();
    let evals = gradient_db::evaluations::with_ids(db, &eval_ids).await?;

    let task_ids: Vec<TaskId> = evals.iter().filter_map(|ev| ev.task).collect();
    let project_ids: std::collections::HashSet<ProjectId> =
        gradient_db::tasks::with_ids(db, &task_ids)
            .await?
            .into_iter()
            .map(|t| t.project)
            .collect();

    for project_id in project_ids {
        if is_project_member(db, user.id, project_id, api_key).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) async fn effective_log_id(
    db: &WebDb,
    shared_build: &MDerivationBuild,
) -> Option<BuildAttemptId> {
    latest_attempt_id(db, shared_build.id).await.ok().flatten()
}
