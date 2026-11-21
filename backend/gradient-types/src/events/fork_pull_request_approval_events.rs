/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, names};
use crate::ids::{ProjectId, PullRequestApprovalId, TaskId};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    NeedsMaintainerApproval,
    EvaluationStartedAfterApproval,
    ApprovalWithoutChangedFileMatchingTriggerPath,
}

impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Phase::NeedsMaintainerApproval => {
                names::FORK_PULL_REQUEST_APPROVAL_NEEDS_MAINTAINER_APPROVAL
            }
            Phase::EvaluationStartedAfterApproval => {
                names::FORK_PULL_REQUEST_APPROVAL_EVALUATION_STARTED_AFTER_APPROVAL
            }
            Phase::ApprovalWithoutChangedFileMatchingTriggerPath => {
                names::FORK_PULL_REQUEST_APPROVAL_APPROVAL_WITHOUT_CHANGED_FILE_MATCHING_TRIGGER_PATH
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    pub approval_id: PullRequestApprovalId,
    pub phase: Phase,
    pub task: TaskId,
    pub project: ProjectId,
    pub pr_number: u64,
    pub pr_author: String,
    pub commit: String,
    pub approving_maintainer_login: Option<String>,
}

impl EventKind for Reported {
    const NAME: &'static str = "fork_pull_request_approval.<phase>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &[
        names::FORK_PULL_REQUEST_APPROVAL_NEEDS_MAINTAINER_APPROVAL,
        names::FORK_PULL_REQUEST_APPROVAL_EVALUATION_STARTED_AFTER_APPROVAL,
        names::FORK_PULL_REQUEST_APPROVAL_APPROVAL_WITHOUT_CHANGED_FILE_MATCHING_TRIGGER_PATH,
    ];

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.phase.name())
    }

    fn owner(&self) -> EventOwner {
        EventOwner::task(self.project, self.task)
    }

    fn key(&self) -> Option<String> {
        Some(format!(
            "fork_pull_request_approval:{}:{}",
            self.approval_id,
            self.phase.name()
        ))
    }
}
