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

pub use self::direct::*;
pub use self::downloads::*;
pub use self::graph::*;
pub use self::log::*;
pub use self::query::*;

use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

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
        build_id: Uuid,
    ) -> WebResult<Self> {
        let build = EBuild::find_by_id(build_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| WebError::not_found("Build"))?;

        let evaluation = EEvaluation::find_by_id(build.evaluation)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Evaluation {} not found for build {}",
                    build.evaluation,
                    build_id
                );
                WebError::InternalServerError("Build data inconsistency".to_string())
            })?;

        let organization_id = if let Some(project_id) = evaluation.project {
            EProject::find_by_id(project_id)
                .one(&state.db)
                .await?
                .ok_or_else(|| {
                    tracing::error!(
                        "Project {} not found for evaluation {}",
                        project_id,
                        evaluation.id
                    );
                    WebError::InternalServerError("Evaluation data inconsistency".to_string())
                })?
                .organization
        } else {
            EDirectBuild::find()
                .filter(CDirectBuild::Evaluation.eq(evaluation.id))
                .one(&state.db)
                .await?
                .ok_or_else(|| {
                    tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
                    WebError::InternalServerError("Direct build data inconsistency".to_string())
                })?
                .organization
        };

        let organization = EOrganization::find_by_id(organization_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("Organization {} not found", organization_id);
                WebError::InternalServerError("Organization data inconsistency".to_string())
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
        build_id: Uuid,
        maybe_user: &Option<MUser>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(state, build_id).await?;

        let can_access = if ctx.organization.public {
            true
        } else {
            match maybe_user {
                Some(user) => user_is_org_member(state, user.id, ctx.organization.id).await?,
                None => false,
            }
        };
        if !can_access {
            return Err(WebError::not_found("Build"));
        }

        Ok(ctx)
    }
}
