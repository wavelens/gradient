/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookResponse {
    pub event: String,
    pub repository_urls: Vec<String>,
    pub tasks_scanned: u32,
    pub queued: Vec<QueuedEvaluation>,
    pub skipped: Vec<SkippedTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedEvaluation {
    pub task_id: TaskId,
    pub task_name: String,
    pub project: String,
    pub evaluation_id: EvaluationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedTask {
    pub task_id: TaskId,
    pub task_name: String,
    pub project: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct WebhookTriggerOutcome {
    pub tasks_scanned: u32,
    pub queued: Vec<QueuedEvaluation>,
    pub skipped: Vec<SkippedTask>,
}

impl WebhookResponse {
    pub fn empty(event: &str) -> Self {
        Self {
            event: event.to_string(),
            repository_urls: Vec::new(),
            tasks_scanned: 0,
            queued: Vec::new(),
            skipped: Vec::new(),
        }
    }
}
