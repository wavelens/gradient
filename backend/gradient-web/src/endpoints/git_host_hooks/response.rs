/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::MTask;
use gradient_types::ids::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IncomingWebhookResponse {
    pub event: String,
    pub repository_urls: Vec<String>,
    pub tasks_scanned: u32,
    pub queued: Vec<QueuedEvaluation>,
    pub skipped: Vec<SkippedTask>,
}

impl IncomingWebhookResponse {
    pub fn new(event: &str, repository_urls: Vec<String>, outcome: WebhookTriggerOutcome) -> Self {
        Self {
            event: event.to_string(),
            repository_urls,
            tasks_scanned: outcome.tasks_scanned,
            queued: outcome.queued,
            skipped: outcome.skipped,
        }
    }

    pub fn empty(event: &str) -> Self {
        Self::new(event, Vec::new(), WebhookTriggerOutcome::default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueuedEvaluation {
    pub task_id: TaskId,
    pub task_name: String,
    pub project: String,
    pub evaluation_id: EvaluationId,
}

impl QueuedEvaluation {
    pub fn new(task: &MTask, project: String, evaluation_id: EvaluationId) -> Self {
        Self {
            task_id: task.id,
            task_name: task.name.clone(),
            project,
            evaluation_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvaluationNotStartedReason {
    SameCommit,
    Filter,
    TagNotFound,
    Concurrency,
    CommitMessage,
    Error,
    InputUpdateRunning,
    InputUpdateNotConfigured,
}

impl TaskEvaluationNotStartedReason {
    pub fn text(self) -> &'static str {
        match self {
            Self::SameCommit => "The task already evaluated this commit.",
            Self::Filter => "Branch not in trigger's branch list",
            Self::TagNotFound => "Tag not found in repository",
            Self::Concurrency => {
                "The concurrency rules of the task skip a new evaluation because an evaluation is already active."
            }
            Self::CommitMessage => "Skip request for CI in the commit message",
            Self::Error => {
                "Gradient failed to start the evaluation. Details are in the server log."
            }
            Self::InputUpdateRunning => {
                "An update of the flake.lock inputs is already in progress."
            }
            Self::InputUpdateNotConfigured => "This task has no setup for updating flake inputs.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SkippedTask {
    pub task_id: TaskId,
    pub task_name: String,
    pub project: String,
    pub reason: TaskEvaluationNotStartedReason,
    pub reason_text: String,
}

impl SkippedTask {
    pub fn new(task: &MTask, project: String, reason: TaskEvaluationNotStartedReason) -> Self {
        Self {
            task_id: task.id,
            task_name: task.name.clone(),
            project,
            reason,
            reason_text: reason.text().into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebhookTriggerOutcome {
    pub tasks_scanned: u32,
    pub queued: Vec<QueuedEvaluation>,
    pub skipped: Vec<SkippedTask>,
}

impl WebhookTriggerOutcome {
    pub(super) fn merge(&mut self, other: Self) {
        self.tasks_scanned += other.tasks_scanned;
        self.queued.extend(other.queued);
        self.skipped.extend(other.skipped);
    }
}
