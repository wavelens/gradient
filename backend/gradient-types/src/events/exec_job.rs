/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, names};
use crate::ids::{EntryPointId, EvaluationId, ExecJobAttemptId, ExecJobId, ProjectId, TaskId};
use gradient_entity::exec_job::ExecJobStatus;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    pub job: ExecJobId,
    pub attempt: Option<ExecJobAttemptId>,
    pub entry_point: EntryPointId,
    pub evaluation: EvaluationId,
    pub status: i16,
    pub phase: Option<String>,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub entry_point_name: Option<String>,
}

impl Reported {
    pub fn reports(status: ExecJobStatus) -> Option<&'static str> {
        match status {
            ExecJobStatus::Queued => Some(names::EXEC_JOB_QUEUED),
            ExecJobStatus::Running => Some(names::EXEC_JOB_STARTED),
            ExecJobStatus::Completed => Some(names::EXEC_JOB_COMPLETED),
            ExecJobStatus::Failed => Some(names::EXEC_JOB_FAILED),
            ExecJobStatus::Aborted => Some(names::EXEC_JOB_ABORTED),
            ExecJobStatus::Created | ExecJobStatus::Starting => None,
        }
    }
}

impl EventKind for Reported {
    const NAME: &'static str = "exec_job.<status>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &[
        names::EXEC_JOB_QUEUED,
        names::EXEC_JOB_STARTED,
        names::EXEC_JOB_PHASE,
        names::EXEC_JOB_COMPLETED,
        names::EXEC_JOB_FAILED,
        names::EXEC_JOB_ABORTED,
    ];

    fn name(&self) -> Cow<'static, str> {
        if self.phase.is_some() {
            return Cow::Borrowed(names::EXEC_JOB_PHASE);
        }
        Cow::Borrowed(
            ExecJobStatus::try_from(self.status)
                .ok()
                .and_then(Self::reports)
                .unwrap_or(names::EXEC_JOB_UNKNOWN),
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
        Some(format!("exec_job:{}:{attempt}:{}", self.job, self.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_name_follows_the_status_or_the_phase() {
        let at = |status: ExecJobStatus| Reported {
            status: i16::from(status),
            ..Default::default()
        };
        assert_eq!(at(ExecJobStatus::Queued).name(), "exec_job.queued");
        assert_eq!(at(ExecJobStatus::Running).name(), "exec_job.started");
        assert_eq!(at(ExecJobStatus::Aborted).name(), "exec_job.aborted");
        assert_eq!(
            Reported {
                phase: Some("epoch-3".into()),
                ..at(ExecJobStatus::Running)
            }
            .name(),
            "exec_job.phase"
        );
        assert_eq!(Reported::reports(ExecJobStatus::Created), None);
    }
}
