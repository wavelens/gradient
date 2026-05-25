/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::types::WaitingReason;
use gradient_core::types::ids::*;
use gradient_core::types::triggers::TriggerType;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeEvaluationRequest {
    pub method: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildItem {
    pub id: BuildId,
    pub name: String,
    pub status: String,
    pub has_artefacts: bool,
    pub updated_at: chrono::NaiveDateTime,
    pub build_time_ms: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct PaginatedBuilds {
    pub builds: Vec<BuildItem>,
    pub total: usize,
    /// Number of builds with status Building, Queued, Failed, Aborted, or DependencyFailed.
    /// The frontend uses this to know how many pages to pre-fetch so all active builds are
    /// in memory (required for correct log streaming and status-transition detection).
    pub active_count: usize,
}

#[derive(Deserialize, Debug, Default)]
pub struct BuildsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Serialize, Debug)]
pub struct EvaluationResponse {
    pub id: EvaluationId,
    pub project: Option<ProjectId>,
    pub project_name: Option<String>,
    pub project_display_name: Option<String>,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub status: entity::evaluation::EvaluationStatus,
    pub previous: Option<EvaluationId>,
    pub next: Option<EvaluationId>,
    pub created_at: chrono::NaiveDateTime,
    pub error_count: u64,
    pub warning_count: u64,
    pub entry_points: Vec<EntryPointBrief>,
    /// `null` for manually-triggered evaluations (Web UI / API), populated for
    /// evaluations that fired from a project trigger (polling, schedule,
    /// reporter push/PR). Mirrors `EvaluationSummary::trigger` on the project
    /// list endpoint so the eval-log "Via" badge can render the same labels
    /// without falling back to "Manual" for trigger-fired runs.
    pub trigger: Option<EvaluationTriggerSummary>,
    /// Populated only when `status == Waiting`. Explains which
    /// `(architecture, required_features)` combos no connected worker can
    /// satisfy, alongside the architectures the connected pool *does* offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
}

/// Trigger that fired an evaluation. Same shape as
/// `crate::endpoints::projects::EvaluationTriggerSummary` - duplicated here to
/// keep the `evals` endpoint module self-contained (the projects module
/// re-exports a long chain of unrelated types).
#[derive(Serialize, Debug)]
pub struct EvaluationTriggerSummary {
    pub id: ProjectTriggerId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
}

/// Compact entry-point representation returned inline on the evaluation.
#[derive(Serialize, Debug)]
pub struct EntryPointBrief {
    pub id: EntryPointId,
    pub eval: String,
    pub build_status: entity::build::BuildStatus,
}

#[derive(Serialize, Debug)]
pub struct EvaluationMessageResponse {
    pub id: EvaluationMessageId,
    pub level: entity::evaluation_message::MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointId>,
}
