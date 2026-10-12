/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, firehose, names};
use crate::ids::{BuildJobId, DerivationBuildId, DerivationId, EvaluationId, ProjectId, TaskId};
use gradient_entity::build::BuildStatus;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BuildProgress {
    pub phase: BuildProgressPhase,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub paths_done: u32,
    pub paths_total: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildProgressPhase {
    Download,
    Prefetch,
    Upload,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusChanged {
    pub build_id: BuildJobId,
    pub derivation_build: DerivationBuildId,
    pub evaluation_id: EvaluationId,
    pub status: i16,
}
firehose!(StatusChanged, names::BUILD_STATUS_CHANGED);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    pub build_id: BuildJobId,
    pub derivation_build: DerivationBuildId,
    pub evaluation_id: EvaluationId,
    pub derivation: DerivationId,
    pub status: i16,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub derivation_path: Option<String>,
    pub evaluation_kind: Option<String>,
}

impl Reported {
    /// No Git host action is consuming `FailedTransient`. A retrying build's check is staying put.
    pub fn reports(status: BuildStatus) -> Option<&'static str> {
        Some(match status {
            BuildStatus::Created => names::BUILD_CREATED,
            BuildStatus::Queued => names::BUILD_QUEUED,
            BuildStatus::Building => names::BUILD_STARTED,
            BuildStatus::Completed => names::BUILD_COMPLETED,
            BuildStatus::FailedPermanent
            | BuildStatus::FailedTimeout
            | BuildStatus::DependencyFailed
            | BuildStatus::Aborted => names::BUILD_FAILED,
            BuildStatus::FailedTransient => names::BUILD_FAILED_TRANSIENT,
            BuildStatus::Substituted => names::BUILD_SUBSTITUTED,
            BuildStatus::Skipped => return None,
        })
    }
}

impl EventKind for Reported {
    const NAME: &'static str = "build.<status>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &[
        names::BUILD_CREATED,
        names::BUILD_QUEUED,
        names::BUILD_STARTED,
        names::BUILD_COMPLETED,
        names::BUILD_FAILED,
        names::BUILD_FAILED_TRANSIENT,
        names::BUILD_SUBSTITUTED,
    ];

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(
            BuildStatus::try_from(i32::from(self.status))
                .ok()
                .and_then(Self::reports)
                .unwrap_or(names::BUILD_UNKNOWN),
        )
    }

    fn owner(&self) -> EventOwner {
        EventOwner {
            project: self.project,
            task: self.task,
            cache: None,
        }
    }

    fn key(&self) -> Option<String> {
        Some(format!("build:{}:{}", self.build_id, self.status))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub derivation_build: DerivationBuildId,
    #[serde(flatten)]
    pub progress: BuildProgress,
}
firehose!(Progress, names::BUILD_PROGRESS);

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(status: BuildStatus) -> Reported {
        Reported {
            status: i16::from(status),
            ..Default::default()
        }
    }

    #[test]
    fn reported_name_follows_status() {
        assert_eq!(reported(BuildStatus::Queued).name(), "build.queued");
        assert_eq!(reported(BuildStatus::Building).name(), "build.started");
        assert_eq!(reported(BuildStatus::Created).name(), "build.created");
        assert_eq!(
            reported(BuildStatus::Substituted).name(),
            "build.substituted"
        );
    }

    #[test]
    fn dependency_failure_and_abort_are_failures() {
        for s in [
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
            BuildStatus::DependencyFailed,
            BuildStatus::Aborted,
        ] {
            assert_eq!(reported(s).name(), "build.failed");
        }
    }

    #[test]
    fn progress_event_flattens_the_phase_and_counts() {
        let event = Progress {
            derivation_build: DerivationBuildId::now_v7(),
            progress: BuildProgress {
                phase: BuildProgressPhase::Prefetch,
                bytes_done: 120,
                bytes_total: Some(340),
                paths_done: 12,
                paths_total: Some(40),
            },
        };

        let json = serde_json::to_value(&event).unwrap();

        assert_eq!(json["phase"], "prefetch");
        assert_eq!(json["bytes_done"], 120);
        assert_eq!(json["bytes_total"], 340);
        assert_eq!(json["paths_done"], 12);
        assert_eq!(json["paths_total"], 40);
    }

    #[test]
    fn skipped_builds_report_nothing() {
        assert!(Reported::reports(BuildStatus::Skipped).is_none());
    }
}
