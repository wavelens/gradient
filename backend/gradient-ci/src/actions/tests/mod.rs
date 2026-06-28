/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::matchers::{forge_status_for_event, matches_event};
use super::payload::{forge_status_payload, render_default_body, render_subject};
use super::report::build_ci_report_from_payload;
use super::truncate;
use gradient_forge::reporter::CiStatus;
use gradient_types::ActionType;
use fixtures::{action_with, make_ctx, run};
use serde_json::json;

#[test]
fn forge_status_mapping() {
    assert!(matches!(
        forge_status_for_event("build.started"),
        Some(CiStatus::Running)
    ));
    assert!(matches!(
        forge_status_for_event("build.completed"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        forge_status_for_event("build.failed"),
        Some(CiStatus::Failure)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.queued"),
        Some(CiStatus::Pending)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.building"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.completed"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.failed"),
        Some(CiStatus::Failure)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.action_required"),
        Some(CiStatus::ActionRequired)
    ));
    assert!(forge_status_for_event("evaluation.waiting").is_none());
}

#[test]
fn matches_event_send_mail_filters_by_stored_events() {
    let a = action_with(ActionType::SendMail, vec!["build.completed"]);
    assert!(matches_event(&a, "build.completed"));
    assert!(!matches_event(&a, "build.failed"));
}

#[test]
fn matches_event_open_pr_fires_only_on_gate_event() {
    use gradient_types::{ActionConfig, IntegrationId, VerifyGate};

    let open_pr = |gate: VerifyGate| {
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
    };

    // The gate keys off the eval's own terminal transition, not a per-build
    // event: a candidate whose closure is already built/substitutable fires no
    // `build.completed`, but the eval still reaches Building/Completed.
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
fn matches_event_forge_status_ignores_stored_events() {
    // The stored `events` list is irrelevant for ForgeStatusReport - the
    // hardcoded FORGE_STATUS_EVENTS set drives matching. Seed the row
    // with an event that is NOT in that set so we can verify the action
    // still fires for every event that IS, regardless of what's stored.
    let a = action_with(ActionType::ForgeStatusReport, vec!["evaluation.waiting"]);
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
    // Not a forge-status event - must not match even though it IS the
    // event we stored on the row.
    assert!(!matches_event(&a, "evaluation.waiting"));
}

#[test]
fn render_subject_with_default_template() {
    let payload = json!({ "project": "demo", "id": "abc" });
    let s = render_subject(None, "build.failed", &payload);
    assert!(s.contains("build.failed"));
    assert!(s.contains("demo"));
}

#[test]
fn render_subject_with_custom_template() {
    let payload = json!({ "project": "demo", "status": "fail" });
    let s = render_subject(Some("X {project} {status}"), "build.failed", &payload);
    assert_eq!(s, "X demo fail");
}

#[test]
fn render_default_body_includes_fields() {
    let payload = json!({
        "org": "o", "project": "p", "id": "i",
        "status": "s", "time": "t", "link": "l",
    });
    let b = render_default_body("build.completed", &payload);
    assert!(b.contains("build.completed"));
    assert!(b.contains("o/p"));
    assert!(b.contains("Link: l"));
}

#[test]
fn truncate_respects_max() {
    let s = "a".repeat(100);
    assert_eq!(truncate(s.clone(), 50).len(), 50);
    assert_eq!(truncate("short".into(), 50), "short");
}

#[test]
fn forge_status_payload_includes_required_fields() {
    let p = forge_status_payload("acme", "widgets", "deadbeef", "ctx", None, None, None);
    assert_eq!(p["owner"], "acme");
    assert_eq!(p["repo"], "widgets");
    assert_eq!(p["sha"], "deadbeef");
    assert_eq!(p["context"], "ctx");
    assert!(p.get("description").is_none());
}

#[test]
fn forge_status_payload_includes_optional_fields() {
    let p = forge_status_payload("o", "r", "s", "c", Some("desc"), Some("https://x"), Some(42));
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
        let err =
            build_ci_report_from_payload(&ctx, "build.started", &payload, CiStatus::Running)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("invalid build_id"), "error: {err}");
    });
}
