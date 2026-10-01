/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Renders `BuildStatus` / `EvaluationStatus` values into raw-SQL fragments so
//! no query hand-writes a status integer. The semantic sets live on the enums
//! in `gradient-entity` (pinned there against renumbering); this module only
//! turns them into `IN (...)` lists and single literals for `format!`-composed
//! statements.

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;

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

/// Comma-joined integer list for `status IN (...)`, e.g. `"4, 6, 9"`.
pub fn build_in(set: &[BuildStatus]) -> String {
    set.iter()
        .map(|s| i32::from(*s).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn eval_in(set: &[EvaluationStatus]) -> String {
    set.iter()
        .map(|s| i32::from(*s).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
