/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::MTask;
use gradient_types::ids::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedTask {
    pub task_id: TaskId,
    pub task_name: String,
    pub project: String,
    pub reason: String,
}

impl SkippedTask {
    pub fn new(task: &MTask, project: String, reason: &str) -> Self {
        Self {
            task_id: task.id,
            task_name: task.name.clone(),
            project,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebhookTriggerOutcome {
    pub tasks_scanned: u32,
    pub queued: Vec<QueuedEvaluation>,
    pub skipped: Vec<SkippedTask>,
}
