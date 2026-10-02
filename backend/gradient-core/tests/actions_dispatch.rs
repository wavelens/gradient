/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_ci::actions::{GIT_HOST_STATUS_EVENTS, git_host_status_payload, matches_event};
use gradient_types::{ActionType, MTaskAction, TaskActionId, TaskId, UserId};
use serde_json::json;
use uuid::Uuid;

fn action_with(action_type: ActionType, events: serde_json::Value) -> MTaskAction {
    MTaskAction {
        id: TaskActionId::now_v7(),
        task: TaskId::new(Uuid::nil()),
        name: "test".into(),
        action_type,
        config: json!({}),
        events,
        active: true,
        last_fired_at: None,
        created_by: UserId::new(Uuid::nil()),
        created_at: chrono::Utc::now().naive_utc(),
        updated_at: chrono::Utc::now().naive_utc(),
    }
}

#[test]
fn matches_event_send_mail_filters_by_events() {
    let action = action_with(
        ActionType::SendMail,
        json!(["build.completed", "build.failed"]),
    );
    assert!(matches_event(&action, "build.completed"));
    assert!(matches_event(&action, "build.failed"));
    assert!(!matches_event(&action, "build.started"));
    assert!(!matches_event(&action, "evaluation.completed"));
}

#[test]
fn matches_event_send_web_request_filters_by_events() {
    let action = action_with(ActionType::SendWebRequest, json!(["evaluation.completed"]));
    assert!(matches_event(&action, "evaluation.completed"));
    assert!(!matches_event(&action, "build.completed"));
}

#[test]
fn matches_event_git_host_status_report_ignores_stored_events() {
    // The stored events are seeded with a non-Git-host-status event. The action must still match
    // every Git-host-status event and must not match the seeded one.
    let action = action_with(
        ActionType::GitHostStatusReport,
        json!(["evaluation.waiting"]),
    );
    for ev in GIT_HOST_STATUS_EVENTS {
        assert!(
            matches_event(&action, ev),
            "git-host-status should always match '{}'",
            ev
        );
    }
    assert!(!matches_event(&action, "evaluation.waiting"));
}

#[test]
fn git_host_status_payload_round_trip_required_fields() {
    let p = git_host_status_payload("acme", "widgets", "deadbeef", "ctx", None, None, None);
    assert_eq!(p["owner"], "acme");
    assert_eq!(p["repo"], "widgets");
    assert_eq!(p["sha"], "deadbeef");
    assert_eq!(p["context"], "ctx");
    assert!(p.get("description").is_none());
    assert!(p.get("details_url").is_none());
    assert!(p.get("check_run_id").is_none());
}

#[test]
fn git_host_status_payload_carries_optional_fields() {
    let p = git_host_status_payload(
        "o",
        "r",
        "s",
        "c",
        Some("desc"),
        Some("https://example.com/log"),
        Some(7),
    );
    assert_eq!(p["description"], "desc");
    assert_eq!(p["details_url"], "https://example.com/log");
    assert_eq!(p["check_run_id"], 7);
}
