/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::tasks::EvaluationTriggerSummary;
use gradient_entity::evaluation::{AbortReason, EvaluationKind};
use gradient_types::ids::*;
use gradient_types::{
    EvaluationProgress, MEntryPoint, MEvaluation, MEvaluationMessage, WaitingReason,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EvaluationAction {
    Abort,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeEvaluationRequest {
    pub method: EvaluationAction,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
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
    pub ifd: bool,
    pub continued_in_evaluation: Option<EvaluationId>,
    pub entry_points: Vec<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct PaginatedBuilds {
    pub builds: Vec<BuildItem>,
    pub total: usize,
    pub active_count: usize,
}

#[derive(Deserialize, Debug, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub scope: Option<BuildJobId>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct EvaluationResponse {
    pub id: EvaluationId,
    pub task: Option<TaskId>,
    pub project_name: String,
    pub task_name: Option<String>,
    pub task_display_name: Option<String>,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub only_evaluate: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_thunks: Option<u64>,
    #[serde(flatten)]
    pub history: EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, utoipa::ToSchema)]
pub struct EvaluationPhaseStartTimestamps {
    pub fetch: Option<chrono::NaiveDateTime>,
    pub eval_flake: Option<chrono::NaiveDateTime>,
    pub eval_derivation: Option<chrono::NaiveDateTime>,
    pub building: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, utoipa::ToSchema)]
pub struct EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration {
    pub abort_reason: Option<AbortReason>,
    pub aborted_by: Option<String>,
    pub superseded_by: Option<EvaluationId>,
    pub retry_of: Option<EvaluationId>,
    pub phase_started_at: EvaluationPhaseStartTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_duration_ms: Option<u64>,
}

impl EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration {
    pub fn new(
        evaluation: &MEvaluation,
        aborted_by: Option<String>,
        expected_duration_ms: Option<u64>,
    ) -> Self {
        Self {
            abort_reason: evaluation.abort_reason,
            aborted_by,
            superseded_by: evaluation.superseded_by,
            retry_of: evaluation
                .previous
                .filter(|_| evaluation.kind == EvaluationKind::Retry),
            phase_started_at: EvaluationPhaseStartTimestamps {
                fetch: evaluation.fetch_started_at,
                eval_flake: evaluation.eval_flake_started_at,
                eval_derivation: evaluation.eval_drv_started_at,
                building: evaluation.building_started_at,
            },
            expected_duration_ms: expected_duration_ms.filter(|_| evaluation.status.is_active()),
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct EntryPointBrief {
    pub id: EntryPointId,
    pub eval: String,
    pub build_status: gradient_entity::build::BuildStatus,
}

impl EntryPointBrief {
    pub fn new(
        entry_point: MEntryPoint,
        build_status: gradient_entity::build::BuildStatus,
    ) -> Self {
        Self {
            id: entry_point.id,
            eval: entry_point.eval,
            build_status,
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct EvaluationMessageResponse {
    pub id: EvaluationMessageId,
    pub level: gradient_entity::evaluation_message::MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointId>,
}

impl EvaluationMessageResponse {
    pub fn new(message: MEvaluationMessage, entry_points: Vec<EntryPointId>) -> Self {
        Self {
            id: message.id,
            level: message.level,
            message: message.message,
            source: message.source,
            created_at: message.created_at,
            entry_points,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(progress: Option<EvaluationProgress>) -> serde_json::Value {
        serde_json::to_value(EvaluationResponse {
            id: EvaluationId::nil(),
            task: None,
            project_name: String::new(),
            task_name: None,
            task_display_name: None,
            repository: String::new(),
            commit: String::new(),
            wildcard: String::new(),
            only_evaluate: false,
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
            expected_thunks: progress.as_ref().map(|_| 450),
            progress,
            history: Default::default(),
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
        assert_eq!(body["expected_thunks"], 450);
        let idle = response(None);
        assert!(idle.get("progress").is_none());
        assert!(idle.get("expected_thunks").is_none());
    }

    #[test]
    fn an_unknown_evaluation_action_is_rejected() {
        let parse = |value| serde_json::from_value::<MakeEvaluationRequest>(value);
        assert_eq!(
            parse(serde_json::json!({ "method": "abort" }))
                .unwrap()
                .method,
            EvaluationAction::Abort
        );
        assert!(parse(serde_json::json!({ "method": "abrot" })).is_err());
    }

    #[test]
    fn evaluation_history_fields_name_retry_carry_abort_reason_and_replacement_and_drop_expected_duration_after_finish()
     {
        use gradient_entity::evaluation::EvaluationStatus;

        let (previous, newer) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let retry = MEvaluation {
            kind: EvaluationKind::Retry,
            previous: Some(previous),
            status: EvaluationStatus::Building,
            ..Default::default()
        };
        let running = EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration::new(
            &retry,
            None,
            Some(600_000),
        );
        assert_eq!(running.retry_of, Some(previous));
        assert_eq!(running.expected_duration_ms, Some(600_000));

        let replaced = MEvaluation {
            previous: Some(previous),
            status: EvaluationStatus::Aborted,
            abort_reason: Some(AbortReason::Superseded),
            superseded_by: Some(newer),
            ..Default::default()
        };
        let body = serde_json::to_value(
            EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration::new(
                &replaced,
                None,
                Some(600_000),
            ),
        )
        .unwrap();
        assert_eq!(body["abort_reason"], "superseded");
        assert_eq!(body["superseded_by"], serde_json::json!(newer));
        assert!(body["retry_of"].is_null());
        assert!(body.get("expected_duration_ms").is_none());
    }
}
