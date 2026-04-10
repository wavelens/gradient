/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod evaluations;
pub mod management;
pub mod metrics;

pub use self::evaluations::*;
pub use self::management::*;
pub use self::metrics::*;

use crate::error::WebError;
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
    pub ci_reporter_type: Option<String>,
    // Base URL of the CI host. Token is intentionally omitted from GET responses.
    pub ci_reporter_url: Option<String>,
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
