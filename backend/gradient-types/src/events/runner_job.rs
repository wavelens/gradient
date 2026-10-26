/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, names};
use crate::ids::{EntryPointId, EvaluationId, ProjectId, RunnerJobAttemptId, RunnerJobId, TaskId};
use gradient_entity::runner_job::RunnerJobStatus;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    pub job: RunnerJobId,
    pub attempt: Option<RunnerJobAttemptId>,
    pub entry_point: EntryPointId,
    pub evaluation: EvaluationId,
    pub status: i16,
    pub phase: Option<String>,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub entry_point_name: Option<String>,
}

impl Reported {
    pub fn reports(status: RunnerJobStatus) -> Option<&'static str> {
        match status {
            RunnerJobStatus::Queued => Some(names::RUNNER_JOB_QUEUED),
            RunnerJobStatus::Running => Some(names::RUNNER_JOB_STARTED),
            RunnerJobStatus::Completed => Some(names::RUNNER_JOB_COMPLETED),
            RunnerJobStatus::Failed => Some(names::RUNNER_JOB_FAILED),
            RunnerJobStatus::Aborted => Some(names::RUNNER_JOB_ABORTED),
            RunnerJobStatus::Created | RunnerJobStatus::Starting => None,
        }
    }
}

impl EventKind for Reported {
    const NAME: &'static str = "runner_job.<status>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &[
        names::RUNNER_JOB_QUEUED,
        names::RUNNER_JOB_STARTED,
        names::RUNNER_JOB_PHASE,
        names::RUNNER_JOB_COMPLETED,
        names::RUNNER_JOB_FAILED,
        names::RUNNER_JOB_ABORTED,
    ];

    fn name(&self) -> Cow<'static, str> {
        if self.phase.is_some() {
            return Cow::Borrowed(names::RUNNER_JOB_PHASE);
        }
        Cow::Borrowed(
            RunnerJobStatus::try_from(self.status)
                .ok()
                .and_then(Self::reports)
                .unwrap_or(names::RUNNER_JOB_UNKNOWN),
        )
    }

    fn owner(&self) -> EventOwner {
        EventOwner {
            project: self.project,
            task: self.task,
            ..EventOwner::default()
        }
    }

    fn key(&self) -> Option<String> {
        if self.phase.is_some() {
            return None;
        }
        let attempt = self.attempt.map(|a| a.to_string()).unwrap_or_default();
        Some(format!("runner_job:{}:{attempt}:{}", self.job, self.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_name_follows_the_status_or_the_phase() {
        let at = |status: RunnerJobStatus| Reported {
            status: i16::from(status),
            ..Default::default()
        };
        assert_eq!(at(RunnerJobStatus::Queued).name(), "runner_job.queued");
        assert_eq!(at(RunnerJobStatus::Running).name(), "runner_job.started");
        assert_eq!(at(RunnerJobStatus::Aborted).name(), "runner_job.aborted");
        assert_eq!(
            Reported {
                phase: Some("epoch-3".into()),
                ..at(RunnerJobStatus::Running)
            }
            .name(),
            "runner_job.phase"
        );
        assert_eq!(Reported::reports(RunnerJobStatus::Created), None);
    }
}
