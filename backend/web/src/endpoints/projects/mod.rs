/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod evaluations;
pub mod integrations;
pub mod management;
pub mod metrics;

pub use self::evaluations::{
    EntryPointDownloadQuery, EntryPointsQuery, EvaluateRequest, get_entry_point_download,
    get_project_details, get_project_entry_points, get_project_evaluations, post_project_evaluate,
};
pub use self::integrations::{
    delete_project_integration, get_project_integration, put_project_integration,
};
pub use self::management::{
    MakeProjectRequest, PatchProjectRequest, TransferOwnershipRequest, delete_project,
    delete_project_active, get, get_project, get_project_name_available, patch_project,
    post_project_active, post_project_check_repository, post_project_transfer, put,
};
pub use self::metrics::{
    EntryPointMetricsQuery, get_entry_point_metrics, get_project_metrics,
};

use uuid::Uuid;
use gradient_core::types::ids::*;

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
    pub build_id: BuildId,
    pub derivation_path: String,
    pub eval: String,
    pub build_status: entity::build::BuildStatus,
    pub has_artefacts: bool,
    pub architecture: entity::server::Architecture,
    pub evaluation_id: EvaluationId,
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

