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
use gradient_entity::evaluation::EvaluationStatus;

pub fn build(status: BuildStatus) -> i32 {
    status.into()
}

pub fn eval(status: EvaluationStatus) -> i32 {
    status.into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_pinned_numbers() {
        assert_eq!(build(BuildStatus::DependencyFailed), 6);
        assert_eq!(eval(EvaluationStatus::Completed), 5);
        assert_eq!(build_in(&BuildStatus::TERMINAL_FAILURE), "4, 6, 9");
        assert_eq!(build_in(&BuildStatus::TERMINAL_SUCCESS), "3, 7");
        assert_eq!(build_in(&BuildStatus::REQUEUEABLE), "4, 5, 6, 9");
        assert_eq!(eval_in(&EvaluationStatus::TERMINAL), "5, 6, 7");
    }
}
