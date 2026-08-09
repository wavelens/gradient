/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Response body types for inbound forge webhooks.

use gradient_types::ids::*;
use serde::{Deserialize, Serialize};

/// Body returned by both webhook endpoints, wrapped in the standard
/// `BaseResponse<T>` envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookResponse {
    /// Forge event name (`push`, `ping`, `installation`, `installation_repositories`,
    /// or the literal string of an unknown GitHub event).
    pub event: String,
    /// Repository URLs extracted from the payload (empty for non-push events).
    pub repository_urls: Vec<String>,
    /// Number of active tasks whose canonicalised URL matched any of
    /// `repository_urls`.
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
    /// One of `already_in_progress`, `no_previous_evaluation`, `db_error`.
    pub reason: String,
}

/// Output of `trigger_for_repo_urls`. Folded into [`WebhookResponse`] by
/// the handler.
#[derive(Debug, Clone, Default)]
pub struct WebhookTriggerOutcome {
    pub tasks_scanned: u32,
    pub queued: Vec<QueuedEvaluation>,
    pub skipped: Vec<SkippedTask>,
}

impl WebhookResponse {
    /// Empty response for events that intentionally don't trigger anything
    /// (ping, installation, unknown).
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
