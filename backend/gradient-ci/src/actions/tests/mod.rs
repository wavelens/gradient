/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod chat;
mod fixtures;
mod summary;

use super::matchers::{git_host_status_for_event, matches_event};
use super::matching_actions;
use super::payload::git_host_status_payload;
use super::report::build_ci_report_from_payload;
use super::truncate;
use fixtures::{action_with, make_ctx, run};
use gradient_git_host::reporter::CiStatus;
use gradient_types::ActionType;
use serde_json::json;

#[test]
fn git_host_status_mapping() {
    assert!(matches!(
        git_host_status_for_event("build.created"),
        Some(CiStatus::Pending)
    ));
    assert!(matches!(
        git_host_status_for_event("build.started"),
        Some(CiStatus::Running)
    ));
    assert!(matches!(
        git_host_status_for_event("build.completed"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        git_host_status_for_event("build.failed"),
        Some(CiStatus::Failure)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.queued"),
        Some(CiStatus::Pending)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.building"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.completed"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.failed"),
        Some(CiStatus::Failure)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.action_required"),
        Some(CiStatus::ActionRequired)
    ));
    assert!(git_host_status_for_event("evaluation.waiting").is_none());
}

#[test]
fn matches_event_send_mail_filters_by_stored_events() {
    let a = action_with(ActionType::SendMail, vec!["build.completed"]);
    assert!(matches_event(&a, "build.completed"));
    assert!(!matches_event(&a, "build.failed"));
}

fn open_pr(gate: gradient_types::VerifyGate) -> gradient_types::MTaskAction {
    use gradient_types::{ActionConfig, IntegrationId};

    let mut a = action_with(ActionType::OpenPr, vec![]);
    a.config = serde_json::to_value(ActionConfig::OpenPr {
        integration_id: IntegrationId::now_v7(),
        generator: Default::default(),
        granularity: Default::default(),
        verify_gate: gate,
        branch_pattern: "gradient/flake-lock-update".into(),
        title_template: None,
        body_template: None,
        update_existing: true,
    })
    .unwrap();

    a
}

#[test]
fn matches_event_open_pr_fires_only_on_gate_event() {
    use gradient_types::VerifyGate;

    // The gate is keying off the evaluation's own terminal transition, not a per-build event. A
    // candidate with an already-built closure is firing no `build.completed`. The evaluation is
    // still reaching `Building` or `Completed`.
    let build_gate = open_pr(VerifyGate::Build);
    assert!(matches_event(&build_gate, "evaluation.completed"));
    assert!(!matches_event(&build_gate, "evaluation.building"));
    assert!(!matches_event(&build_gate, "build.completed"));
    assert!(!matches_event(&build_gate, "evaluation.failed"));

    let eval_gate = open_pr(VerifyGate::Eval);
    assert!(matches_event(&eval_gate, "evaluation.building"));
    assert!(!matches_event(&eval_gate, "evaluation.completed"));
    assert!(!matches_event(&eval_gate, "build.completed"));
}

#[test]
fn matches_event_git_host_status_ignores_stored_events() {
    // The stored `events` list is irrelevant for `GitHostStatusReport` because
    // `GIT_HOST_STATUS_EVENTS` is driving matching. The row is seeded with an event outside that
    // set on purpose.
    let a = action_with(ActionType::GitHostStatusReport, vec!["evaluation.waiting"]);
    assert!(matches_event(&a, "build.created"));
    assert!(matches_event(&a, "build.queued"));
    assert!(matches_event(&a, "build.started"));
    assert!(matches_event(&a, "build.completed"));
    assert!(matches_event(&a, "build.failed"));
    assert!(matches_event(&a, "build.substituted"));
    assert!(matches_event(&a, "evaluation.queued"));
    assert!(matches_event(&a, "evaluation.building"));
    assert!(matches_event(&a, "evaluation.completed"));
    assert!(matches_event(&a, "evaluation.action_required"));
    assert!(matches_event(&a, "evaluation.approval_granted"));
    assert!(!matches_event(&a, "evaluation.waiting"));
}

#[test]
fn truncate_respects_max() {
    let s = "a".repeat(100);
    assert_eq!(truncate(s.clone(), 50).len(), 50);
    assert_eq!(truncate("short".into(), 50), "short");
}

#[test]
fn git_host_status_payload_includes_required_fields() {
    let p = git_host_status_payload("acme", "widgets", "deadbeef", "ctx", None, None, None);
    assert_eq!(p["owner"], "acme");
    assert_eq!(p["repo"], "widgets");
    assert_eq!(p["sha"], "deadbeef");
    assert_eq!(p["context"], "ctx");
    assert!(p.get("description").is_none());
}

#[test]
fn git_host_status_payload_includes_optional_fields() {
    let p = git_host_status_payload(
        "o",
        "r",
        "s",
        "c",
        Some("desc"),
        Some("https://x"),
        Some(42),
    );
    assert_eq!(p["description"], "desc");
    assert_eq!(p["details_url"], "https://x");
    assert_eq!(p["check_run_id"], 42);
}

#[test]
fn build_ci_report_fast_path_uses_payload_fields() {
    run(async {
        let ctx = make_ctx();
        let payload = json!({
            "owner": "acme",
            "repo": "widgets",
            "sha": "deadbeef",
            "context": "gradient/my-pkg",
            "description": "Building…",
            "details_url": "https://example.com/log/1",
            "check_run_id": 99,
        });
        let report =
            build_ci_report_from_payload(&ctx, "build.started", &payload, CiStatus::Running)
                .await
                .expect("fast path should succeed")
                .expect("fast path always emits a report");
        assert_eq!(report.owner, "acme");
        assert_eq!(report.repo, "widgets");
        assert_eq!(report.sha, "deadbeef");
        assert_eq!(report.context, "gradient/my-pkg");
        assert_eq!(report.description.as_deref(), Some("Building…"));
        assert_eq!(report.existing_check_id, Some(99));
    });
}

#[test]
fn build_ci_report_errors_when_payload_empty() {
    run(async {
        let ctx = make_ctx();
        let err =
            build_ci_report_from_payload(&ctx, "build.started", &json!({}), CiStatus::Running)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("build_id"), "error: {err}");
    });
}

#[test]
fn build_ci_report_errors_on_invalid_build_id() {
    run(async {
        let ctx = make_ctx();
        let payload = json!({ "build_id": "not-a-uuid" });
        let err = build_ci_report_from_payload(&ctx, "build.started", &payload, CiStatus::Running)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid build_id"), "error: {err}");
    });
}

#[test]
fn an_input_update_reaches_open_pr_and_never_the_git_host_report() {
    let actions = vec![
        open_pr(gradient_types::VerifyGate::Build),
        action_with(ActionType::GitHostStatusReport, vec![]),
        action_with(ActionType::SendMail, vec!["evaluation.completed"]),
    ];
    let payload = json!({"evaluation_kind": "input_update"});

    let matched = matching_actions(actions, "evaluation.completed", &payload);

    assert_eq!(
        matched.iter().map(|a| a.action_type).collect::<Vec<_>>(),
        vec![ActionType::OpenPr, ActionType::SendMail],
    );
}

#[test]
fn a_normal_run_reaches_the_git_host_report_and_never_open_pr() {
    let actions = vec![
        open_pr(gradient_types::VerifyGate::Build),
        action_with(ActionType::GitHostStatusReport, vec![]),
    ];
    let payload = json!({"evaluation_kind": "normal"});

    let matched = matching_actions(actions, "evaluation.completed", &payload);

    assert_eq!(
        matched.iter().map(|a| a.action_type).collect::<Vec<_>>(),
        vec![ActionType::GitHostStatusReport],
    );
}

#[test]
fn an_action_that_does_not_subscribe_is_not_matched() {
    let actions = vec![action_with(ActionType::SendMail, vec!["build.failed"])];

    assert!(matching_actions(actions, "build.completed", &json!({})).is_empty());
}
