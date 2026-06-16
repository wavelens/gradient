/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_forge::reporter::{APPROVAL_ACTION_ID, CiStatus, RequestedAction};
use gradient_types::{ActionConfig, ActionType, MProjectAction, VerifyGate};

pub const FORGE_STATUS_EVENTS: &[&str] = &[
    "build.queued",
    "build.started",
    "build.completed",
    "build.failed",
    "build.substituted",
    "evaluation.queued",
    "evaluation.started",
    "evaluation.completed",
    "evaluation.failed",
    "evaluation.aborted",
    "evaluation.action_required",
    "evaluation.approval_granted",
];

pub fn matches_event(action: &MProjectAction, event: &str) -> bool {
    if action.action_type == ActionType::ForgeStatusReport.to_i16() {
        return FORGE_STATUS_EVENTS.contains(&event);
    }
    if action.action_type == ActionType::OpenPr.to_i16() {
        return open_pr_gate_event(action) == Some(event);
    }
    action
        .events
        .as_array()
        .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(event)))
}

/// The single verify-gate event an `OpenPr` action fires on. The dispatcher
/// additionally restricts firing to `input_update` evaluations.
pub fn open_pr_gate_event(action: &MProjectAction) -> Option<&'static str> {
    let cfg: ActionConfig = serde_json::from_value(action.config.clone()).ok()?;
    let ActionConfig::OpenPr { verify_gate, .. } = cfg else {
        return None;
    };

    Some(match verify_gate {
        VerifyGate::Build => "build.completed",
        VerifyGate::Eval | VerifyGate::None => "evaluation.completed",
    })
}

pub fn forge_status_for_event(event: &str) -> Option<CiStatus> {
    match event {
        "build.queued" => Some(CiStatus::Pending),
        "build.started" => Some(CiStatus::Running),
        "build.completed" => Some(CiStatus::Success),
        "build.failed" => Some(CiStatus::Failure),
        "build.substituted" => Some(CiStatus::Success),
        "evaluation.queued" => Some(CiStatus::Pending),
        "evaluation.started" => Some(CiStatus::Running),
        "evaluation.completed" => Some(CiStatus::Success),
        "evaluation.failed" => Some(CiStatus::Failure),
        "evaluation.aborted" => Some(CiStatus::Error),
        "evaluation.action_required" => Some(CiStatus::ActionRequired),
        // Approval click clears the Awaiting-Approval gate. We post Success
        // on the same check so the maintainer sees it turn green and the
        // PR's required-checks count drops the gate.
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
