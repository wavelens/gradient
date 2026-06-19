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

use crate::access::is_org_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use gradient_db::latest_attempt_id;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

/// Resolved access context for a per-eval build (`build_job`).
///
/// The public build identity is the `build_job` id; build state lives on the
/// shared `derivation_build` anchor. Walks build_job -> evaluation -> project ->
/// organization and enforces the access check. Returns `not_found("Build")` on
/// any failure so callers cannot distinguish missing from forbidden.
pub(super) struct BuildAccessContext {
    pub build_job: MBuildJob,
    pub anchor: MDerivationBuild,
    pub organization: MOrganization,
}

impl BuildAccessContext {
    /// Load build_job + anchor + organization without enforcing an access check.
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

        let project_id = evaluation.project.ok_or_else(|| {
            tracing::warn!(evaluation_id = %evaluation.id, "evaluation has no project");
            WebError::data_inconsistency("Evaluation")
        })?;
        let organization_id = EProject::find_by_id(project_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    %project_id,
                    evaluation_id = %evaluation.id,
                    "Project not found for evaluation",
                );
                WebError::data_inconsistency("Evaluation")
            })?
            .organization;

        let organization = EOrganization::find_by_id(organization_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(%organization_id, "Organization not found");
                WebError::data_inconsistency("Organization")
            })?;

        Ok(Self {
            build_job,
            anchor,
            organization,
        })
    }

    /// Load build_job + organization and enforce public/member access.
    ///
    /// Returns `not_found("Build")` when the build does not exist, the
    /// organization is private, and `maybe_user` is neither a direct member nor
    /// a member of another org whose evaluations also reference the derivation.
    pub(super) async fn load(
        state: &Arc<ServerState>,
        build_job_id: BuildJobId,
        maybe_user: &Option<MUser>,
        api_key: Option<&ApiKeyContext>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(state, build_job_id).await?;

        let direct_access = if ctx.organization.public {
            true
        } else {
            match maybe_user {
                Some(user) => is_org_member(state, user.id, ctx.organization.id, api_key).await?,
                None => false,
            }
        };
        if direct_access {
            return Ok(ctx);
        }

        if let Some(user) = maybe_user
            && reachable_orgs_accessible(state, user, api_key, ctx.build_job.derivation).await?
        {
            return Ok(ctx);
        }

        Err(WebError::not_found("Build"))
    }
}

/// True when `user` belongs to any org whose evaluations also reference
/// `derivation` (a `build_job` exists for it in that org). The derivation is
/// global and content-addressed, so any org that built it may read its log.
async fn reachable_orgs_accessible(
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

    let mut org_ids: std::collections::HashSet<OrganizationId> = std::collections::HashSet::new();
    for ev in evals {
        let Some(project_id) = ev.project else {
            continue;
        };
        if let Some(p) = EProject::find_by_id(project_id).one(&state.web_db).await? {
            org_ids.insert(p.organization);
        }
    }

    for org_id in org_ids {
        if is_org_member(state, user.id, org_id, api_key).await? {
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
    latest_attempt_id(&state.web_db, anchor.id).await.ok().flatten()
}
