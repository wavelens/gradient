/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod evaluations;
pub mod flake_inputs;
pub mod integrations;
pub mod management;
pub mod metrics;
pub mod triggers;

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
pub use self::metrics::{EntryPointMetricsQuery, get_entry_point_metrics, get_project_metrics};

use gradient_core::types::ids::*;

// ── Shared types ─────────────────────────────────────────────────────────────

use entity::evaluation::EvaluationStatus;
use gradient_core::types::triggers::ConcurrencyPolicy;
use gradient_core::types::{ProjectTriggerId, TriggerType};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectResponse {
    pub id: ProjectId,
    pub organization: OrganizationId,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub last_evaluation: Option<EvaluationId>,
    pub last_evaluation_status: Option<EvaluationStatus>,
    pub force_evaluation: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub concurrency: ConcurrencyPolicy,
    pub sign_cache: bool,
    /// Caller holds `Permission::EditProject` — may edit project configuration.
    pub can_edit: bool,
    /// Caller holds `Permission::TriggerEvaluation` — may start/restart/abort
    /// evaluations. Distinct from `can_edit` so users granted only trigger
    /// rights can act, and so managed projects (which reject config edits)
    /// still expose trigger actions when the backend permits them.
    pub can_trigger: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointSummary {
    pub id: EntryPointId,
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
pub struct EvaluationTriggerSummary {
    pub id: ProjectTriggerId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EvaluationSummary {
    pub id: EvaluationId,
    pub commit: String,
    pub status: EvaluationStatus,
    pub trigger: Option<EvaluationTriggerSummary>,
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
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub active: bool,
    pub created_at: chrono::NaiveDateTime,
    pub keep_evaluations: i32,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
    pub can_trigger: bool,
    pub managed: bool,
}
