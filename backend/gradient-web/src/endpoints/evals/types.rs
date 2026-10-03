/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::*;
use gradient_types::triggers::TriggerType;
use gradient_types::{EvaluationProgress, WaitingReason};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeEvaluationRequest {
    pub method: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildItem {
    pub id: BuildJobId,
    pub name: String,
    pub status: String,
    pub has_artefacts: bool,
    pub updated_at: chrono::NaiveDateTime,
    pub build_time_ms: Option<i64>,
    pub build_started_at: Option<chrono::NaiveDateTime>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub depth: u32,
    pub prioritized: bool,
}

#[derive(Serialize, Debug)]
pub struct PaginatedBuilds {
    pub builds: Vec<BuildItem>,
    pub total: usize,
    pub active_count: usize,
}

#[derive(Deserialize, Debug, Default)]
pub struct BuildsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub scope: Option<BuildJobId>,
}

#[derive(Serialize, Debug)]
pub struct EvaluationResponse {
    pub id: EvaluationId,
    pub task: Option<TaskId>,
    pub task_name: Option<String>,
    pub task_display_name: Option<String>,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub status: gradient_entity::evaluation::EvaluationStatus,
    pub previous: Option<EvaluationId>,
    pub next: Option<EvaluationId>,
    pub created_at: chrono::NaiveDateTime,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub updated_at: chrono::NaiveDateTime,
    pub error_count: u64,
    pub warning_count: u64,
    pub error: Option<String>,
    pub entry_points: Vec<EntryPointBrief>,
    pub prioritized: bool,
    pub trigger: Option<EvaluationTriggerSummary>,
    pub triggered_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<EvaluationProgress>,
}

#[derive(Serialize, Debug)]
pub struct EvaluationTriggerSummary {
    pub id: TaskTriggerId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
}

#[derive(Serialize, Debug)]
pub struct EntryPointBrief {
    pub id: EntryPointId,
    pub eval: String,
    pub build_status: gradient_entity::build::BuildStatus,
}

#[derive(Serialize, Debug)]
pub struct EvaluationMessageResponse {
    pub id: EvaluationMessageId,
    pub level: gradient_entity::evaluation_message::MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(progress: Option<EvaluationProgress>) -> serde_json::Value {
        serde_json::to_value(EvaluationResponse {
            id: EvaluationId::nil(),
            task: None,
            task_name: None,
            task_display_name: None,
            repository: String::new(),
            commit: String::new(),
            wildcard: String::new(),
            status: gradient_entity::evaluation::EvaluationStatus::EvaluatingFlake,
            previous: None,
            next: None,
            created_at: chrono::NaiveDateTime::default(),
            started_at: None,
            finished_at: None,
            updated_at: chrono::NaiveDateTime::default(),
            error_count: 0,
            warning_count: 0,
            error: None,
            entry_points: vec![],
            prioritized: false,
            trigger: None,
            triggered_by: None,
            waiting_reason: None,
            progress,
        })
        .unwrap()
    }

    #[test]
    fn the_body_carries_progress_only_while_there_is_some() {
        let body = response(Some(EvaluationProgress::Evaluating { thunks: 7 }));
        assert_eq!(
            body["progress"],
            serde_json::json!({ "kind": "evaluating", "thunks": 7 })
        );
        assert!(response(None).get("progress").is_none());
    }
}
