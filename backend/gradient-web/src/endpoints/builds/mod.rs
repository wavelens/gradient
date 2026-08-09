/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod closure;
pub mod downloads;
pub mod graph;
pub mod log;
pub mod log_chunks;
pub mod query;

pub use self::closure::{
    ClosureEdge, ClosureGraph, ClosureNode, build_closure_graph, build_runtime_closure_graph,
    derivation_closure_reachable, get_build_closure, get_build_runtime_closure, get_eval_closure,
    get_eval_runtime_closure, sum_output_sizes,
};
pub use self::downloads::{
    BuildProduct, DownloadQuery, get_build_download, get_build_download_token, get_build_downloads,
};
pub use self::graph::{
    BuildGraph, DependencyEdge, DependencyNode, get_build_dependencies, get_build_graph,
};
pub use self::log::{get_build_log, post_build_log};
pub use self::log_chunks::{
    get_build_log_chunk, get_build_log_chunks, get_build_log_lines, get_build_log_search,
};
pub use self::query::{BuildWithOutputs, get_build};

use crate::access::is_project_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use gradient_core::ServerState;
use gradient_db::latest_attempt_id;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

/// Resolved access context for a per-eval build (`build_job`).
///
/// The public build identity is the `build_job` id; build state lives on the
/// shared `derivation_build` anchor. Walks build_job -> evaluation -> task ->
/// project and enforces the access check. Returns `not_found("Build")` on
/// any failure so callers cannot distinguish missing from forbidden.
pub(super) struct BuildAccessContext {
    pub build_job: MBuildJob,
    pub anchor: MDerivationBuild,
    pub project: MProject,
}

impl BuildAccessContext {
    /// Load build_job + anchor + project without enforcing an access check.
    ///
    /// Use this when access is gated by custom logic (e.g. download tokens).
    pub(super) async fn load_unguarded(
        state: &Arc<ServerState>,
        build_job_id: BuildJobId,
    ) -> WebResult<Self> {
        let build_job = EBuildJob::find_by_id(build_job_id)
            .one(&state.web_db)
            .await?
            .or_not_found("Build")?;

        let anchor = EDerivationBuild::find_by_id(build_job.derivation_build)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    anchor_id = %build_job.derivation_build,
                    build_job_id = %build_job_id,
                    "DerivationBuild anchor not found for build_job",
                );
                WebError::data_inconsistency("Build")
            })?;

        let evaluation = EEvaluation::find_by_id(build_job.evaluation)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    evaluation_id = %build_job.evaluation,
                    build_job_id = %build_job_id,
                    "Evaluation not found for build_job",
                );
                WebError::data_inconsistency("Build")
            })?;

        let task_id = evaluation.task.ok_or_else(|| {
            tracing::warn!(evaluation_id = %evaluation.id, "evaluation has no task");
            WebError::data_inconsistency("Evaluation")
        })?;
        let project_id = ETask::find_by_id(task_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    %task_id,
                    evaluation_id = %evaluation.id,
                    "Task not found for evaluation",
                );
                WebError::data_inconsistency("Evaluation")
            })?
            .project;

        let project = EProject::find_by_id(project_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(%project_id, "Project not found");
                WebError::data_inconsistency("Project")
            })?;

        Ok(Self {
            build_job,
            anchor,
            project,
        })
    }

    /// Load build_job + project and enforce public/member access.
    ///
    /// Returns `not_found("Build")` when the build does not exist, the
    /// project is private, and `maybe_user` is neither a direct member nor
    /// a member of another project whose evaluations also reference the derivation.
    pub(super) async fn load(
        state: &Arc<ServerState>,
        build_job_id: BuildJobId,
        maybe_user: &Option<MUser>,
        api_key: Option<&ApiKeyContext>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(state, build_job_id).await?;

        let direct_access = if ctx.project.public {
            true
        } else {
            match maybe_user {
                Some(user) => is_project_member(state, user.id, ctx.project.id, api_key).await?,
                None => false,
            }
        };
        if direct_access {
            return Ok(ctx);
        }

        if let Some(user) = maybe_user
            && reachable_projects_accessible(state, user, api_key, ctx.build_job.derivation).await?
        {
            return Ok(ctx);
        }

        Err(WebError::not_found("Build"))
    }
}

/// True when `user` belongs to any project whose evaluations also reference
/// `derivation` (a `build_job` exists for it in that project). The derivation is
/// global and content-addressed, so any project that built it may read its log.
async fn reachable_projects_accessible(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
    derivation: DerivationId,
) -> WebResult<bool> {
    let jobs = gradient_db::build_jobs_for_derivation(&state.web_db, derivation).await?;
    if jobs.is_empty() {
        return Ok(false);
    }

    let eval_ids: Vec<EvaluationId> = jobs.into_iter().map(|j| j.evaluation).collect();
    let evals = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(eval_ids))
        .all(&state.web_db)
        .await?;

    let mut project_ids: std::collections::HashSet<ProjectId> = std::collections::HashSet::new();
    for ev in evals {
        let Some(task_id) = ev.task else {
            continue;
        };
        if let Some(p) = ETask::find_by_id(task_id).one(&state.web_db).await? {
            project_ids.insert(p.project);
        }
    }

    for project_id in project_ids {
        if is_project_member(state, user.id, project_id, api_key).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The attempt id whose stored log should be served for an anchor: its latest
/// attempt. Substituted/cache-completed anchors may never have produced an
/// attempt, in which case there is no log to read.
pub(super) async fn effective_log_id(
    state: &Arc<ServerState>,
    anchor: &MDerivationBuild,
) -> Option<BuildAttemptId> {
    latest_attempt_id(&state.web_db, anchor.id)
        .await
        .ok()
        .flatten()
}
