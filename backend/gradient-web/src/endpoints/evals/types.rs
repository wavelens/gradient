/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_api_types::{
    BuildItem, EntryPointBrief, EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration,
    EvaluationAction, EvaluationMessageResponse, EvaluationPhaseStartTimestamps,
    EvaluationResponse, MakeEvaluationRequest, PaginatedBuilds,
};
use gradient_entity::evaluation::EvaluationKind;
use gradient_types::ids::*;
use gradient_types::{MEntryPoint, MEvaluation, MEvaluationMessage};
use serde::Deserialize;

#[derive(Deserialize, Debug, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub scope: Option<BuildJobId>,
}

pub fn new_evaluation_abort_replacement_retry_phase_starts_and_expected_duration(
    evaluation: &MEvaluation,
    aborted_by: Option<String>,
    expected_duration_ms: Option<u64>,
) -> EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration {
    EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration {
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

pub fn entry_point_brief_from_row_and_build_status(
    entry_point: MEntryPoint,
    build_status: gradient_entity::build::BuildStatus,
) -> EntryPointBrief {
    EntryPointBrief {
        id: entry_point.id,
        eval: entry_point.eval,
        build_status,
    }
}

pub fn evaluation_message_response_from_message_row_and_entry_point_ids(
    message: MEvaluationMessage,
    entry_points: Vec<EntryPointId>,
) -> EvaluationMessageResponse {
    EvaluationMessageResponse {
        id: message.id,
        level: message.level,
        message: message.message,
        source: message.source,
        created_at: message.created_at,
        entry_points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::AbortReason;
    use gradient_types::EvaluationProgress;

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
        let running = new_evaluation_abort_replacement_retry_phase_starts_and_expected_duration(
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
            new_evaluation_abort_replacement_retry_phase_starts_and_expected_duration(
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
