/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `gradient_core::ci::actions`.
//!
//! Async assertions use sync `#[test]` + `tokio::runtime::Builder::block_on`.

use gradient_core::ci::actions::{
    FORGE_STATUS_EVENTS, forge_status_for_event, forge_status_payload, matches_event,
};
use gradient_core::ci::CiStatus;
use gradient_core::types::{ActionType, MProjectAction, ProjectActionId, ProjectId, UserId};
use serde_json::json;
use uuid::Uuid;

fn action_with(action_type: ActionType, events: serde_json::Value) -> MProjectAction {
    MProjectAction {
        id: ProjectActionId::now_v7(),
        project: ProjectId::new(Uuid::nil()),
        name: "test".into(),
        action_type: action_type.to_i16(),
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
fn matches_event_forge_status_report_ignores_stored_events() {
    // Seed the stored events with something that is NOT in
    // FORGE_STATUS_EVENTS so we can verify the action still matches every
    // forge-status event (proving the stored list is disregarded) and does
    // NOT match the unrelated event it was seeded with.
    let action = action_with(ActionType::ForgeStatusReport, json!(["evaluation.waiting"]));
    for ev in FORGE_STATUS_EVENTS {
        assert!(
            matches_event(&action, ev),
            "forge-status should always match '{}'",
            ev
        );
    }
    assert!(!matches_event(&action, "evaluation.waiting"));
}

#[test]
fn forge_status_mapping_complete() {
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
        forge_status_for_event("build.substituted"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.queued"),
        Some(CiStatus::Pending)
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
        forge_status_for_event("evaluation.aborted"),
        Some(CiStatus::Error)
    ));
    assert!(matches!(
        forge_status_for_event("evaluation.action_required"),
        Some(CiStatus::ActionRequired)
    ));
    assert!(forge_status_for_event("evaluation.waiting").is_none());
}

#[test]
fn forge_status_payload_round_trip_required_fields() {
    let p = forge_status_payload("acme", "widgets", "deadbeef", "ctx", None, None, None);
    assert_eq!(p["owner"], "acme");
    assert_eq!(p["repo"], "widgets");
    assert_eq!(p["sha"], "deadbeef");
    assert_eq!(p["context"], "ctx");
    assert!(p.get("description").is_none());
    assert!(p.get("details_url").is_none());
    assert!(p.get("check_run_id").is_none());
}

#[test]
fn forge_status_payload_carries_optional_fields() {
    let p = forge_status_payload(
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
