/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod evaluations;
pub mod integrations;
pub mod management;
pub mod metrics;

pub use self::evaluations::*;
pub use self::integrations::*;
pub use self::management::*;
pub use self::metrics::*;

use crate::endpoints::get_org_readable;
use crate::error::{WebError, WebResult};
use core::db::get_project_by_name;
use core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

// ── Shared types ─────────────────────────────────────────────────────────────

use entity::evaluation::EvaluationStatus;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub last_evaluation: Option<Uuid>,
    pub last_evaluation_status: Option<EvaluationStatus>,
    pub force_evaluation: bool,
    pub created_by: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub can_edit: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointSummary {
    pub id: Uuid,
    pub build_id: Uuid,
    pub derivation_path: String,
    pub eval: String,
    pub build_status: entity::build::BuildStatus,
    pub has_artefacts: bool,
    pub architecture: entity::server::Architecture,
    pub evaluation_id: Uuid,
    pub evaluation_status: EvaluationStatus,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EvaluationSummary {
    pub id: Uuid,
    pub commit: String,
    pub status: EvaluationStatus,
    pub total_builds: i64,
    pub failed_builds: i64,
    pub completed_entry_points: i64,
    pub failed_entry_points: i64,
    pub entry_point_diff: Option<i64>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectDetailsResponse {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub active: bool,
    pub created_at: chrono::NaiveDateTime,
    pub keep_evaluations: i32,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Load a project in an org that is readable by `maybe_user`.
///
/// Uses `get_org_readable` so private orgs are invisible to non-members.
/// Returns `not_found("Project")` when the org is inaccessible or the project
/// doesn't exist.
pub(crate) async fn load_readable_project(
    state: &Arc<ServerState>,
    maybe_user: &Option<MUser>,
    org_name: String,
    project_name: String,
) -> WebResult<(MOrganization, MProject)> {
    let organization = get_org_readable(state, org_name, maybe_user, "Project").await?;
    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project_name))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;
    Ok((organization, project))
}

/// Load a project by (org_name, project_name) that the given user is a member of.
///
/// Returns `not_found("Project")` when the org or project doesn't exist, or
/// when the user is not a member of the org.
pub(crate) async fn load_project(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
    project_name: String,
) -> WebResult<(MOrganization, MProject)> {
    get_project_by_name(Arc::clone(state), user_id, org_name, project_name)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))
}

/// Load an editable project: the user must have edit (Admin/Write) permission and
/// the project must not be state-managed.
pub(crate) async fn load_editable_project(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
    project_name: String,
) -> WebResult<(MOrganization, MProject)> {
    let (organization, project) = load_project(state, user_id, org_name, project_name).await?;

    if !user_can_edit(state, user_id, organization.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to modify this project.".to_string(),
        ));
    }

    if project.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API.".to_string(),
        ));
    }

    Ok((organization, project))
}

/// Returns true if the user has Admin or Write role in the organization.
pub(crate) async fn user_can_edit(
    state: &Arc<ServerState>,
    user_id: Uuid,
    organization_id: Uuid,
) -> Result<bool, WebError> {
    use core::types::consts::*;
    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.db)
        .await?;

    Ok(match org_user {
        Some(ou) => ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID,
        None => false,
    })
}
