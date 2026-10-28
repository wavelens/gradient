/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_git_host::reporter::{APPROVAL_ACTION_ID, CiStatus, RequestedAction};
use gradient_types::events::names;
use gradient_types::{ActionConfig, ActionType, MTaskAction, PatchGeneratorKind, VerifyGate};

pub const GIT_HOST_STATUS_EVENTS: &[&str] = &[
    names::BUILD_CREATED,
    names::BUILD_QUEUED,
    names::BUILD_STARTED,
    names::BUILD_COMPLETED,
    names::BUILD_FAILED,
    names::BUILD_SUBSTITUTED,
    names::EVALUATION_QUEUED,
    names::EVALUATION_STARTED,
    names::EVALUATION_BUILDING,
    names::EVALUATION_COMPLETED,
    names::EVALUATION_FAILED,
    names::EVALUATION_ABORTED,
    names::EVALUATION_ACTION_REQUIRED,
    names::EVALUATION_APPROVAL_GRANTED,
    names::EXEC_JOB_STARTED,
    names::EXEC_JOB_COMPLETED,
    names::EXEC_JOB_FAILED,
    names::EXEC_JOB_ABORTED,
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
    const BUILD_GATE: &[&str] = &[names::EVALUATION_COMPLETED];
    const EVAL_GATE: &[&str] = &[names::EVALUATION_BUILDING];
    const EXEC_TREE_GATE: &[&str] = &[names::EXEC_JOB_COMPLETED];

    let cfg: ActionConfig = serde_json::from_value(action.config.clone()).ok()?;
    let ActionConfig::OpenPr {
        verify_gate,
        generator,
        ..
    } = cfg
    else {
        return None;
    };

    Some(match (generator, verify_gate) {
        (PatchGeneratorKind::ExecTree, _) => EXEC_TREE_GATE,
        (PatchGeneratorKind::FlakeLock, VerifyGate::Build) => BUILD_GATE,
        (PatchGeneratorKind::FlakeLock, VerifyGate::Eval | VerifyGate::None) => EVAL_GATE,
    })
}

pub fn git_host_status_for_event(event: &str) -> Option<CiStatus> {
    match event {
        names::BUILD_CREATED => Some(CiStatus::Pending),
        names::BUILD_QUEUED => Some(CiStatus::Pending),
        names::BUILD_STARTED => Some(CiStatus::Running),
        names::BUILD_COMPLETED => Some(CiStatus::Success),
        names::BUILD_FAILED => Some(CiStatus::Failure),
        names::BUILD_SUBSTITUTED => Some(CiStatus::Success),
        names::EVALUATION_QUEUED => Some(CiStatus::Pending),
        names::EVALUATION_STARTED => Some(CiStatus::Running),
        names::EVALUATION_BUILDING => Some(CiStatus::Success),
        names::EVALUATION_COMPLETED => Some(CiStatus::Success),
        names::EVALUATION_FAILED => Some(CiStatus::Failure),
        names::EVALUATION_ABORTED => Some(CiStatus::Error),
        names::EVALUATION_ACTION_REQUIRED => Some(CiStatus::ActionRequired),
        names::EVALUATION_APPROVAL_GRANTED => Some(CiStatus::Success),
        names::EXEC_JOB_STARTED => Some(CiStatus::Running),
        names::EXEC_JOB_COMPLETED => Some(CiStatus::Success),
        names::EXEC_JOB_FAILED => Some(CiStatus::Failure),
        names::EXEC_JOB_ABORTED => Some(CiStatus::Error),
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
