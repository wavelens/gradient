/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::transition::Transition;

pub fn to<S: Into<i32> + Copy>(transition: Transition<S>) -> i32 {
    transition.to.into()
}

pub fn from<S: Into<i32> + Copy>(transition: Transition<S>) -> String {
    transition
        .from
        .iter()
        .map(|status| (*status).into().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn build(status: BuildStatus) -> i32 {
    status.into()
}

pub fn eval(status: EvaluationStatus) -> i32 {
    status.into()
}

pub fn attempt_outcome(outcome: AttemptOutcome) -> i32 {
    outcome.into()
}

pub fn attempt_reason(reason: AttemptFailureReason) -> i32 {
    reason.into()
}

pub fn build_in(set: &[BuildStatus]) -> String {
    set.iter()
        .map(|s| i32::from(*s).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn exec(status: ExecJobStatus) -> i16 {
    status.into()
}

pub fn exec_in(set: &[ExecJobStatus]) -> String {
    set.iter()
        .map(|s| i16::from(*s).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn eval_in(set: &[EvaluationStatus]) -> String {
    set.iter()
        .map(|s| i32::from(*s).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
