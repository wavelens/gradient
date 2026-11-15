/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_git_host::reporter::{APPROVAL_ACTION_ID, CiStatus, RequestedAction};
use gradient_types::events::names;
use gradient_types::{ActionConfig, ActionType, MTaskAction, PatchGeneratorKind, VerifyGate};

pub fn matches_event(action: &MTaskAction, event: &str) -> bool {
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
