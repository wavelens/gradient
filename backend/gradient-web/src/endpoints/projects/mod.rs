/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
mod auto_attach;
pub mod evaluations;
pub mod flake_inputs;
pub mod management;
pub mod metrics;
pub mod triggers;

pub use self::evaluations::{
    EntryPointDownloadQuery, EntryPointsQuery, EvaluateRequest, EvaluationsQuery,
    get_entry_point_download, get_project_details, get_project_entry_points,
    get_project_evaluations, post_project_evaluate,
};
pub use self::management::{
    MakeProjectRequest, PatchProjectRequest, TransferOwnershipRequest, delete_project,
    delete_project_active, get, get_project, get_project_name_available, patch_project,
    post_project_active, post_project_check_repository, post_project_transfer, put,
};
pub use self::metrics::{EntryPointMetricsQuery, get_entry_point_metrics, get_project_metrics};

use gradient_types::ids::*;

// ── Shared types ─────────────────────────────────────────────────────────────

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::triggers::ConcurrencyPolicy;
use gradient_types::{ProjectTriggerId, TriggerType};
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
    /// Caller holds `Permission::EditProject` - may edit project configuration.
    pub can_edit: bool,
    /// Caller holds `Permission::TriggerEvaluation` - may start/restart/abort
    /// evaluations. Distinct from `can_edit` so users granted only trigger
    /// rights can act, and so managed projects (which reject config edits)
    /// still expose trigger actions when the backend permits them.
    pub can_trigger: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointSummary {
    pub id: EntryPointId,
    /// Per-eval build identity (`build_job` id) for this entry point's derivation.
    pub build_id: BuildJobId,
    pub derivation_path: String,
    pub eval: String,
    pub build_status: gradient_entity::build::BuildStatus,
    pub has_artefacts: bool,
    pub architecture: gradient_entity::server::Architecture,
    pub build_time_ms: Option<i64>,
    pub deps: BuildStatusCounts,
    /// Total build-time dependency-closure size of this entry point, cached on
    /// the derivation (content-addressed, reused across evals). `null` for evals
    /// predating the cache.
    pub deps_total: Option<i64>,
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
    pub commit_message: Option<String>,
    pub status: EvaluationStatus,
    pub trigger: Option<EvaluationTriggerSummary>,
    pub triggered_by: Option<String>,
    /// PR/MR number for pull-request-triggered evaluations, for the "PR #42"
    /// label and forge link. `None` for non-PR triggers.
    pub pr_number: Option<u64>,
    pub total_builds: i64,
    pub builds: BuildStatusCounts,
    pub errors: i64,
    pub warnings: i64,
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
    pub last_check_at: Option<chrono::NaiveDateTime>,
    pub queue: QueueSummary,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
    pub can_trigger: bool,
    pub managed: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BarSegment {
    Completed,
    Failed,
    Building,
    Queued,
    Substituted,
    Aborted,
}

pub fn bar_segment(status: BuildStatus) -> BarSegment {
    use BuildStatus::*;
    match status {
        Completed => BarSegment::Completed,
        FailedPermanent | FailedTimeout | DependencyFailed => BarSegment::Failed,
        Building => BarSegment::Building,
        Queued | Created | FailedTransient => BarSegment::Queued,
        Substituted => BarSegment::Substituted,
        Aborted => BarSegment::Aborted,
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy)]
pub struct BuildStatusCounts {
    pub completed: i64,
    pub failed: i64,
    pub building: i64,
    pub queued: i64,
    pub substituted: i64,
    pub aborted: i64,
}

impl BuildStatusCounts {
    pub fn add(&mut self, status: BuildStatus, n: i64) {
        match bar_segment(status) {
            BarSegment::Completed => self.completed += n,
            BarSegment::Failed => self.failed += n,
            BarSegment::Building => self.building += n,
            BarSegment::Queued => self.queued += n,
            BarSegment::Substituted => self.substituted += n,
            BarSegment::Aborted => self.aborted += n,
        }
    }

    /// Sum of the four drawn segments; excludes `substituted` and `aborted`.
    pub fn total(&self) -> i64 {
        self.completed + self.failed + self.building + self.queued
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy)]
pub struct QueueSummary {
    pub building: i64,
    pub queued: i64,
}

#[cfg(test)]
mod rollup_tests {
    use super::{BarSegment, BuildStatusCounts, bar_segment};
    use gradient_entity::build::BuildStatus;

    #[test]
    fn segment_mapping_matches_spec() {
        use BuildStatus::*;
        assert_eq!(bar_segment(Completed), BarSegment::Completed);
        for s in [FailedPermanent, FailedTimeout, DependencyFailed] {
            assert_eq!(bar_segment(s), BarSegment::Failed);
        }
        assert_eq!(bar_segment(Building), BarSegment::Building);
        for s in [Queued, Created, FailedTransient] {
            assert_eq!(bar_segment(s), BarSegment::Queued);
        }
        assert_eq!(bar_segment(Substituted), BarSegment::Substituted);
        assert_eq!(bar_segment(Aborted), BarSegment::Aborted);
    }

    #[test]
    fn total_excludes_substituted_and_aborted() {
        let mut c = BuildStatusCounts::default();
        c.add(BuildStatus::Completed, 3);
        c.add(BuildStatus::FailedPermanent, 2);
        c.add(BuildStatus::Building, 1);
        c.add(BuildStatus::Queued, 4);
        c.add(BuildStatus::Substituted, 9000);
        c.add(BuildStatus::Aborted, 5);
        assert_eq!(c.completed, 3);
        assert_eq!(c.failed, 2);
        assert_eq!(c.building, 1);
        assert_eq!(c.queued, 4);
        assert_eq!(c.substituted, 9000);
        assert_eq!(c.aborted, 5);
        assert_eq!(c.total(), 10);
    }
}
