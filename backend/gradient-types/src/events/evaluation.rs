/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, firehose, names};
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

impl From<EvaluationStatus> for Phase {
    fn from(status: EvaluationStatus) -> Self {
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
}

impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Queued => names::EVALUATION_QUEUED,
            Phase::Started => names::EVALUATION_STARTED,
            Phase::Building => names::EVALUATION_BUILDING,
            Phase::Waiting => names::EVALUATION_WAITING,
            Phase::Completed => names::EVALUATION_COMPLETED,
            Phase::Failed => names::EVALUATION_FAILED,
            Phase::Aborted => names::EVALUATION_ABORTED,
            Phase::ActionRequired => names::EVALUATION_ACTION_REQUIRED,
            Phase::ApprovalGranted => names::EVALUATION_APPROVAL_GRANTED,
        }
    }

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
        names::EVALUATION_QUEUED,
        names::EVALUATION_STARTED,
        names::EVALUATION_BUILDING,
        names::EVALUATION_WAITING,
        names::EVALUATION_COMPLETED,
        names::EVALUATION_FAILED,
        names::EVALUATION_ABORTED,
        names::EVALUATION_ACTION_REQUIRED,
        names::EVALUATION_APPROVAL_GRANTED,
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
firehose!(Progress, names::EVALUATION_PROGRESS);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvaluationProgress {
    Fetching { inputs: Vec<InputFetch> },
    Evaluating { thunks: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFetch {
    pub name: String,
    pub state: InputFetchState,
    pub downloaded_bytes: u64,
    pub expected_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputFetchState {
    Queued,
    Fetching,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub evaluation_id: EvaluationId,
    pub task: Option<TaskId>,
    pub progress: EvaluationProgress,
}
firehose!(Activity, names::EVALUATION_ACTIVITY);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_and_eval_phases_report_started() {
        assert_eq!(
            Phase::from(EvaluationStatus::Fetching).name(),
            "evaluation.started"
        );
        assert_eq!(
            Phase::from(EvaluationStatus::EvaluatingDerivation).name(),
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
