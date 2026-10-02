/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_git_host::reporter::{APPROVAL_ACTION_ID, CiStatus, RequestedAction};
use gradient_types::{ActionConfig, ActionType, MTaskAction, VerifyGate};

pub const GIT_HOST_STATUS_EVENTS: &[&str] = &[
    "build.created",
    "build.queued",
    "build.started",
    "build.completed",
    "build.failed",
    "build.substituted",
    "evaluation.queued",
    "evaluation.started",
    "evaluation.building",
    "evaluation.completed",
    "evaluation.failed",
    "evaluation.aborted",
    "evaluation.action_required",
    "evaluation.approval_granted",
];

pub fn matches_event(action: &MTaskAction, event: &str) -> bool {
    if action.action_type == ActionType::GitHostStatusReport {
        return GIT_HOST_STATUS_EVENTS.contains(&event);
    }
    if action.action_type == ActionType::OpenPr {
        return open_pr_gate_events(action).is_some_and(|evs| evs.contains(&event));
    }
    action
        .events
        .as_array()
        .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(event)))
}

/// An already-built closure is firing no `build.completed`. The gate is keying off evaluation
/// transitions instead. `Build` is waiting for `evaluation.completed` because a failed build is
/// leaving the evaluation `Failed` without an event. `Eval` and `None` are opening at
/// `evaluation.building`.
pub fn open_pr_gate_events(action: &MTaskAction) -> Option<&'static [&'static str]> {
    const BUILD_GATE: &[&str] = &["evaluation.completed"];
    const EVAL_GATE: &[&str] = &["evaluation.building"];

    let cfg: ActionConfig = serde_json::from_value(action.config.clone()).ok()?;
    let ActionConfig::OpenPr { verify_gate, .. } = cfg else {
        return None;
    };

    Some(match verify_gate {
        VerifyGate::Build => BUILD_GATE,
        VerifyGate::Eval | VerifyGate::None => EVAL_GATE,
    })
}

pub fn git_host_status_for_event(event: &str) -> Option<CiStatus> {
    match event {
        "build.created" => Some(CiStatus::Pending),
        "build.queued" => Some(CiStatus::Pending),
        "build.started" => Some(CiStatus::Running),
        "build.completed" => Some(CiStatus::Success),
        "build.failed" => Some(CiStatus::Failure),
        "build.substituted" => Some(CiStatus::Success),
        "evaluation.queued" => Some(CiStatus::Pending),
        "evaluation.started" => Some(CiStatus::Running),
        "evaluation.building" => Some(CiStatus::Success),
        "evaluation.completed" => Some(CiStatus::Success),
        "evaluation.failed" => Some(CiStatus::Failure),
        "evaluation.aborted" => Some(CiStatus::Error),
        "evaluation.action_required" => Some(CiStatus::ActionRequired),
        "evaluation.approval_granted" => Some(CiStatus::Success),
        _ => None,
    }
}

pub(super) fn requested_actions_for(status: CiStatus) -> Vec<RequestedAction> {
    match status {
        CiStatus::ActionRequired => vec![RequestedAction {
            identifier: APPROVAL_ACTION_ID.to_string(),
            label: "Approve and run".to_string(),
            description: "Run CI for external contributor PR.".to_string(),
        }],
        _ => Vec::new(),
    }
}
