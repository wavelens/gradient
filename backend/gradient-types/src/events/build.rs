/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, firehose};
use crate::ids::{BuildJobId, DerivationBuildId, DerivationId, EvaluationId, ProjectId, TaskId};
use gradient_entity::build::BuildStatus;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusChanged {
    pub build_id: BuildJobId,
    pub derivation_build: DerivationBuildId,
    pub evaluation_id: EvaluationId,
    pub status: i16,
}
firehose!(StatusChanged, "build.status_changed");

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
            BuildStatus::Created => "build.created",
            BuildStatus::Queued => "build.queued",
            BuildStatus::Building => "build.started",
            BuildStatus::Completed => "build.completed",
            BuildStatus::FailedPermanent
            | BuildStatus::FailedTimeout
            | BuildStatus::DependencyFailed
            | BuildStatus::Aborted => "build.failed",
            BuildStatus::FailedTransient => "build.failed_transient",
            BuildStatus::Substituted => "build.substituted",
            BuildStatus::Skipped => return None,
        })
    }
}

impl EventKind for Reported {
    const NAME: &'static str = "build.<status>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &[
        "build.created",
        "build.queued",
        "build.started",
        "build.completed",
        "build.failed",
        "build.failed_transient",
        "build.substituted",
    ];

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(
            BuildStatus::try_from(i32::from(self.status))
                .ok()
                .and_then(Self::reports)
                .unwrap_or("build.unknown"),
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
    pub progress: DownloadProgress,
}
firehose!(Progress, "build.progress");

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(status: BuildStatus) -> Reported {
        Reported {
            status: i32::from(status) as i16,
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
    fn skipped_builds_report_nothing() {
        assert!(Reported::reports(BuildStatus::Skipped).is_none());
    }
}
