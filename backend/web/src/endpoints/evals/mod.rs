/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod log;
pub mod query;
pub mod types;

pub use self::actions::*;
pub use self::log::*;
pub use self::query::*;
pub use self::types::*;

use crate::access::is_org_member;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

/// Resolved access context for an evaluation.
///
/// Loaded once per request: fetches the evaluation row, resolves the owning
/// organization (through project or direct_build), and enforces the access
/// check.  Returns `not_found("Evaluation")` on any failure so callers cannot
/// distinguish missing from forbidden.
pub(super) struct EvalAccessContext {
    pub evaluation: MEvaluation,
    pub organization_id: OrganizationId,
    /// Only set when the eval is linked to a project (not a direct build).
    pub project_name: Option<String>,
    pub project_display_name: Option<String>,
}

impl EvalAccessContext {
    pub(super) async fn load(
        state: &Arc<ServerState>,
        evaluation_id: EvaluationId,
        maybe_user: &Option<MUser>,
    ) -> WebResult<Self> {
        let evaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&state.web_db)
            .await?
            .or_not_found("Evaluation")?;

        let (organization_id, project_name, project_display_name) =
            if let Some(project_id) = evaluation.project {
                let project = EProject::find_by_id(project_id)
                    .one(&state.web_db)
                    .await?
                    .ok_or_else(|| {
                        tracing::error!(
                            %project_id,
                            %evaluation_id,
                            "Project not found for evaluation",
                        );
                        WebError::data_inconsistency("Evaluation")
                    })?;
                (
                    project.organization,
                    Some(project.name),
                    Some(project.display_name),
                )
            } else {
                let org_id = EDirectBuild::find()
                    .filter(CDirectBuild::Evaluation.eq(evaluation.id))
                    .one(&state.web_db)
                    .await?
                    .ok_or_else(|| {
                        tracing::error!(%evaluation_id, "DirectBuild not found for evaluation");
                        WebError::data_inconsistency("Direct build")
                    })?
                    .organization;
                (org_id, None, None)
            };

        let organization = EOrganization::find_by_id(organization_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::error!(%organization_id, "Organization not found");
                WebError::data_inconsistency("Organization")
            })?;

        let can_access = if organization.public {
            true
        } else {
            match maybe_user {
                Some(user) => is_org_member(state, user.id, organization.id).await?,
                None => false,
            }
        };
        if !can_access {
            return Err(WebError::not_found("Evaluation"));
        }

        Ok(Self {
            evaluation,
            organization_id,
            project_name,
            project_display_name,
        })
    }
}
