/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_ci::actions::matches_event;
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
