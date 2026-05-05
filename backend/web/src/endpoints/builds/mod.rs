/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod direct;
pub mod downloads;
pub mod graph;
pub mod log;
pub mod query;

pub use self::direct::{
    DirectBuildInfo, DirectBuildRequest, get_recent_direct_builds, post_direct_build,
};
pub use self::downloads::{
    BuildProduct, DownloadQuery, get_build_download, get_build_download_token, get_build_downloads,
};
pub use self::graph::{
    BuildGraph, DependencyEdge, DependencyNode, get_build_dependencies, get_build_graph,
};
pub use self::log::{get_build_log, post_build_log};
pub use self::query::{BuildWithOutputs, get_build};

use crate::helpers::OptionExt;
use crate::access::is_org_member;
use crate::error::{WebError, WebResult};
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

/// Resolved access context for a build.
///
/// Walks up build → evaluation → project/direct_build → organization and
/// enforces the access check.  Returns `not_found("Build")` on any failure
/// so callers cannot distinguish missing from forbidden.
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
                tracing::error!(
                    "Evaluation {} not found for build {}",
                    build.evaluation,
                    build_id
                );
                WebError::data_inconsistency("Build")
            })?;

        let organization_id = if let Some(project_id) = evaluation.project {
            EProject::find_by_id(project_id)
                .one(&state.web_db)
                .await?
                .ok_or_else(|| {
                    tracing::error!(
                        "Project {} not found for evaluation {}",
                        project_id,
                        evaluation.id
                    );
                    WebError::data_inconsistency("Evaluation")
                })?
                .organization
        } else {
            EDirectBuild::find()
                .filter(CDirectBuild::Evaluation.eq(evaluation.id))
                .one(&state.web_db)
                .await?
                .ok_or_else(|| {
                    tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
                    WebError::data_inconsistency("Direct build")
                })?
                .organization
        };

        let organization = EOrganization::find_by_id(organization_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::error!("Organization {} not found", organization_id);
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
    /// organization is private, and `maybe_user` is not a member.
    pub(super) async fn load(
        state: &Arc<ServerState>,
        build_id: BuildId,
        maybe_user: &Option<MUser>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(state, build_id).await?;

        let can_access = if ctx.organization.public {
            true
        } else {
            match maybe_user {
                Some(user) => is_org_member(state, user.id, ctx.organization.id).await?,
                None => false,
            }
        };
        if !can_access {
            return Err(WebError::not_found("Build"));
        }

        Ok(ctx)
    }
}
