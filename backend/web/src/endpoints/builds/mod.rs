/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod downloads;
pub mod graph;
pub mod log;
pub mod query;

pub use self::downloads::{
    BuildProduct, DownloadQuery, get_build_download, get_build_download_token, get_build_downloads,
};
pub use self::graph::{
    BuildGraph, DependencyEdge, DependencyNode, get_build_dependencies, get_build_graph,
};
pub use self::log::{get_build_log, post_build_log};
pub use self::query::{BuildWithOutputs, get_build};

use crate::access::is_org_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

/// Resolved access context for a build.
///
/// Walks up build → evaluation → project → organization and enforces the
/// access check. Returns `not_found("Build")` on any failure so callers
/// cannot distinguish missing from forbidden.
pub(super) struct BuildAccessContext {
    pub build: MBuild,
    pub organization: MOrganization,
}

impl BuildAccessContext {
    /// Load build + organization without enforcing an access check.
    ///
    /// Use this when access is gated by custom logic (e.g. download tokens).
    pub(super) async fn load_unguarded(
        state: &Arc<ServerState>,
        build_id: BuildId,
    ) -> WebResult<Self> {
        let build = EBuild::find_by_id(build_id)
            .one(&state.web_db)
            .await?
            .or_not_found("Build")?;

        let evaluation = EEvaluation::find_by_id(build.evaluation)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    evaluation_id = %build.evaluation,
                    %build_id,
                    "Evaluation not found for build",
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
            build,
            organization,
        })
    }

    /// Load build + organization and enforce public/member access.
    ///
    /// Returns `not_found("Build")` when the build does not exist, the
    /// organization is private, and `maybe_user` is neither a direct member
    /// nor a member of any follower-org that points at this build via `via`.
    pub(super) async fn load(
        state: &Arc<ServerState>,
        build_id: BuildId,
        maybe_user: &Option<MUser>,
        api_key: Option<&ApiKeyContext>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(state, build_id).await?;

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
            && follower_orgs_accessible(state, user, api_key, build_id).await?
        {
            return Ok(ctx);
        }

        Err(WebError::not_found("Build"))
    }
}

async fn follower_orgs_accessible(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
    leader_build_id: BuildId,
) -> WebResult<bool> {
    let follower_builds: Vec<MBuild> = EBuild::find()
        .filter(CBuild::Via.eq(leader_build_id))
        .all(&state.web_db)
        .await?;
    if follower_builds.is_empty() {
        return Ok(false);
    }

    let follower_eval_ids: Vec<EvaluationId> =
        follower_builds.into_iter().map(|b| b.evaluation).collect();

    let evals = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(follower_eval_ids))
        .all(&state.web_db)
        .await?;

    let mut org_ids: std::collections::HashSet<OrganizationId> =
        std::collections::HashSet::new();
    for ev in evals {
        let Some(project_id) = ev.project else {
            continue;
        };
        if let Some(p) = EProject::find_by_id(project_id)
            .one(&state.web_db)
            .await?
        {
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
