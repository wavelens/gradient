/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure-logic helpers for naming CI check rows and mapping internal
//! evaluation/build statuses to the [`CiStatus`] reported via Actions.

use crate::ci::CiStatus;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;

/// `"{org}/{project}"` when both are known, falling back to `"{project}"` when
/// the organization lookup turned up nothing. Used as the scope segment of
/// every CI check name so multiple Gradient projects reporting to the same
/// repository remain distinguishable.
pub fn format_check_scope(org_name: Option<&str>, project_name: &str) -> String {
    match org_name {
        Some(org) => format!("{}/{}", org, project_name),
        None => project_name.to_string(),
    }
}

/// CI check name for the maintainer-approval gate (Awaiting Approval).
pub fn approval_check_context(project_name: &str) -> String {
    format!("gradient/{}: Awaiting Approval", project_name)
}

/// CI check name for the per-evaluation roll-up status.
pub fn evaluation_check_context(project_name: &str) -> String {
    format!("gradient/{}: Evaluation", project_name)
}

/// CI check name for a single entry-point build under an evaluation.
pub fn build_check_context(project_name: &str, entry_point: &str) -> String {
    format!("gradient/{}: Build {}", project_name, entry_point)
}

/// Map an event name to the check-context family it reports to.
/// Used by the reporter to pick the right slot in `evaluation.check_run_ids`
/// so the Approval, Evaluation, and Build checks each get their own check_run_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckContextKind {
    /// `Awaiting Approval` gate, cleared when a maintainer approves.
    Approval,
    /// Per-evaluation roll-up status (Queued → Running → terminal).
    Evaluation,
    /// Per-entry-point build status.
    Build,
}

/// Classify a dispatch event into the check-context family it should report to.
pub fn check_context_kind_for_event(event: &str) -> Option<CheckContextKind> {
    match event {
        "evaluation.action_required" => Some(CheckContextKind::Approval),
        "evaluation.approval_granted" => Some(CheckContextKind::Approval),
        "evaluation.queued"
        | "evaluation.started"
        | "evaluation.completed"
        | "evaluation.failed"
        | "evaluation.aborted" => Some(CheckContextKind::Evaluation),
        "build.started" | "build.completed" | "build.failed" | "build.substituted" => {
            Some(CheckContextKind::Build)
        }
        _ => None,
    }
}

/// Maps an [`EvaluationStatus`] to the [`CiStatus`] reported to external forges.
///
/// Returns `None` for non-terminal/intermediate states that do not produce a
/// CI report from this helper (the per-job handlers report `Running` directly
/// when an evaluation starts).
pub fn ci_status_for_evaluation(status: &EvaluationStatus) -> Option<CiStatus> {
    match status {
        EvaluationStatus::Completed => Some(CiStatus::Success),
        EvaluationStatus::Failed => Some(CiStatus::Failure),
        EvaluationStatus::Aborted => Some(CiStatus::Error),
        EvaluationStatus::Queued
        | EvaluationStatus::Fetching
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation
        | EvaluationStatus::Building
        | EvaluationStatus::Waiting => None,
    }
}

/// Maps a [`BuildStatus`] to the [`CiStatus`] reported per-entry-point.
///
/// Returns `None` for non-terminal states; the per-eval-name `Pending` is
/// reported once at evaluation time.
pub fn ci_status_for_build(status: &BuildStatus) -> Option<CiStatus> {
    match status {
        BuildStatus::Building => Some(CiStatus::Running),
        BuildStatus::Completed | BuildStatus::Substituted => Some(CiStatus::Success),
        BuildStatus::Failed | BuildStatus::DependencyFailed => Some(CiStatus::Failure),
        BuildStatus::Aborted => Some(CiStatus::Error),
        BuildStatus::Created | BuildStatus::Queued => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_scope_with_org() {
        assert_eq!(
            format_check_scope(Some("wavelens"), "my-project"),
            "wavelens/my-project"
        );
    }

    #[test]
    fn check_scope_without_org_falls_back_to_project() {
        assert_eq!(format_check_scope(None, "my-project"), "my-project");
    }

    #[test]
    fn approval_context_format() {
        assert_eq!(
            approval_check_context("my-project"),
            "gradient/my-project: Awaiting Approval"
        );
    }

    #[test]
    fn evaluation_context_format() {
        assert_eq!(
            evaluation_check_context("my-project"),
            "gradient/my-project: Evaluation"
        );
    }

    #[test]
    fn build_context_format() {
        assert_eq!(
            build_check_context("my-project", "my-package"),
            "gradient/my-project: Build my-package"
        );
    }

    #[test]
    fn maps_terminal_states() {
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Failed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Aborted),
            Some(CiStatus::Error)
        );
    }

    #[test]
    fn maps_build_terminal_states() {
        assert_eq!(
            ci_status_for_build(&BuildStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Substituted),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Failed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::DependencyFailed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Aborted),
            Some(CiStatus::Error)
        );
    }

    #[test]
    fn skips_intermediate_build_states() {
        for s in [BuildStatus::Created, BuildStatus::Queued] {
            assert_eq!(ci_status_for_build(&s), None);
        }
    }

    #[test]
    fn maps_building_to_running() {
        assert_eq!(
            ci_status_for_build(&BuildStatus::Building),
            Some(CiStatus::Running)
        );
    }

    #[test]
    fn skips_intermediate_states() {
        for s in [
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ] {
            assert_eq!(ci_status_for_evaluation(&s), None);
        }
    }
}
