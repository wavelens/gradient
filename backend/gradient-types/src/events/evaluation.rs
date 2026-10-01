/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, firehose};
use crate::ids::{EvaluationId, ProjectId, TaskId};
use crate::waiting_reason::WaitingReason;
use gradient_entity::evaluation::EvaluationStatus;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Queued,
    Started,
    Building,
    Waiting,
    Completed,
    Failed,
    Aborted,
    ActionRequired,
    ApprovalGranted,
}

impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Queued => "evaluation.queued",
            Phase::Started => "evaluation.started",
            Phase::Building => "evaluation.building",
            Phase::Waiting => "evaluation.waiting",
            Phase::Completed => "evaluation.completed",
            Phase::Failed => "evaluation.failed",
            Phase::Aborted => "evaluation.aborted",
            Phase::ActionRequired => "evaluation.action_required",
            Phase::ApprovalGranted => "evaluation.approval_granted",
        }
    }

    pub fn of_status(status: EvaluationStatus) -> Self {
        match status {
            EvaluationStatus::Queued => Phase::Queued,
            EvaluationStatus::Fetching
            | EvaluationStatus::EvaluatingFlake
            | EvaluationStatus::EvaluatingDerivation => Phase::Started,
            EvaluationStatus::Building => Phase::Building,
            EvaluationStatus::Waiting => Phase::Waiting,
            EvaluationStatus::Completed => Phase::Completed,
            EvaluationStatus::Failed => Phase::Failed,
            EvaluationStatus::Aborted => Phase::Aborted,
        }
    }

    /// The first report of a freshly inserted evaluation and the description its Git host check shows.
    pub fn of_created(
        status: EvaluationStatus,
        reason: Option<WaitingReason>,
    ) -> Option<(Self, Option<&'static str>)> {
        Some(match (status, reason) {
            (EvaluationStatus::Queued, _) => (Phase::Queued, None),
            (EvaluationStatus::Waiting, Some(WaitingReason::Approval { .. })) => (
                Phase::ActionRequired,
                Some("Awaiting maintainer approval for external contributor PR."),
            ),
            (EvaluationStatus::Waiting, Some(WaitingReason::NoCache)) => (
                Phase::Queued,
                Some("Waiting for a writable cache subscription before this evaluation can run."),
            ),
            (EvaluationStatus::Waiting, Some(WaitingReason::CacheStorageFull)) => (
                Phase::Queued,
                Some("Waiting for cache storage to free up before this evaluation can run."),
            ),
            (
                EvaluationStatus::Waiting,
                Some(WaitingReason::Workers {
                    connected_workers: 0,
                    ..
                }),
            ) => (
                Phase::Queued,
                Some("Waiting for an eval-capable worker to be registered on the project."),
            ),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    pub evaluation_id: EvaluationId,
    pub phase: Phase,
    pub status: i16,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub repository: Option<String>,
    pub evaluation_kind: Option<String>,
    pub description: Option<String>,
    pub created: bool,
}

impl EventKind for Reported {
    const NAME: &'static str = "evaluation.<phase>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &[
        "evaluation.queued",
        "evaluation.started",
        "evaluation.building",
        "evaluation.waiting",
        "evaluation.completed",
        "evaluation.failed",
        "evaluation.aborted",
        "evaluation.action_required",
        "evaluation.approval_granted",
    ];

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.phase.name())
    }

    fn owner(&self) -> EventOwner {
        EventOwner {
            project: self.project,
            task: self.task,
            cache: None,
        }
    }

    fn key(&self) -> Option<String> {
        Some(format!(
            "evaluation:{}:{}:{}",
            self.evaluation_id,
            self.status,
            self.phase.name()
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub evaluation_id: EvaluationId,
    pub task: Option<TaskId>,
}
firehose!(Progress, "evaluation.progress");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_and_eval_phases_report_started() {
        assert_eq!(
            Phase::of_status(EvaluationStatus::Fetching).name(),
            "evaluation.started"
        );
        assert_eq!(
            Phase::of_status(EvaluationStatus::EvaluatingDerivation).name(),
            "evaluation.started"
        );
    }

    #[test]
    fn a_created_evaluation_waiting_for_approval_requires_action() {
        let (phase, description) = Phase::of_created(
            EvaluationStatus::Waiting,
            Some(WaitingReason::Approval {
                pr_number: 1,
                pr_author: "someone".into(),
            }),
        )
        .unwrap();
        assert_eq!(phase.name(), "evaluation.action_required");
        assert!(description.is_some());
    }

    #[test]
    fn a_created_evaluation_that_owes_no_report_has_none() {
        assert!(Phase::of_created(EvaluationStatus::Building, None).is_none());
    }
}
