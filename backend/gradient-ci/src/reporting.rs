/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure-logic helpers for naming CI check rows and mapping internal
//! evaluation/build statuses to the [`CiStatus`] reported via Actions.

use crate::CiStatus;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;

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

/// CI check name for the maintainer-approval gate.
pub fn approval_check_context(project_name: &str) -> String {
    format!("gradient/{}: Approval", project_name)
}

/// CI check name for the per-evaluation roll-up status. `wildcard_suffix` is
/// `Some` only when a run targets a wildcard other than the project default
/// (e.g. `/gradient run <wildcard>`), so that custom-wildcard runs report as
/// their own check line instead of overwriting the default evaluation check.
pub fn evaluation_check_context(project_name: &str, wildcard_suffix: Option<&str>) -> String {
    match wildcard_suffix {
        Some(w) => format!("gradient/{}: Evaluation: {}", project_name, w),
        None => format!("gradient/{}: Evaluation", project_name),
    }
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
        | "evaluation.building"
        | "evaluation.completed"
        | "evaluation.failed"
        | "evaluation.aborted" => Some(CheckContextKind::Evaluation),
        "build.queued"
        | "build.started"
        | "build.completed"
        | "build.failed"
        | "build.substituted" => Some(CheckContextKind::Build),
        _ => None,
    }
}

/// Whether a forge report for the Evaluation check should be suppressed.
///
/// The Evaluation check tracks the evaluation phase, which concludes
/// successfully the moment the eval reaches `Building`. A later `Failure`/
/// `Error` is a build-phase failure or a post-build abort - surfaced by the
/// per-Build checks - so it must not redden an already-green Evaluation check
/// once `Building` has been reached.
pub fn suppress_evaluation_failure(status: &CiStatus, reached_building: bool) -> bool {
    reached_building && matches!(status, CiStatus::Failure | CiStatus::Error)
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
        BuildStatus::FailedPermanent
            | BuildStatus::FailedTimeout
            | BuildStatus::DependencyFailed => Some(CiStatus::Failure),
        BuildStatus::Aborted => Some(CiStatus::Error),
        BuildStatus::Created | BuildStatus::Queued | BuildStatus::FailedTransient => None,
    }
}

/// The dispatch event a per-entry-point build transition reports, or `None` for
/// statuses that produce no CI check: the initial `Created`, and `FailedTransient`
/// (a build that will retry, so its check stays put). `Queued`/`Building` post the
/// live Pending/Running progress; a dependency failure or an abort surface as a
/// failed check so a build that already posted Pending/Running resolves rather
/// than hanging.
pub fn build_event_for_status(status: BuildStatus) -> Option<&'static str> {
    Some(match status {
        BuildStatus::Queued => "build.queued",
        BuildStatus::Building => "build.started",
        BuildStatus::Completed => "build.completed",
        BuildStatus::FailedPermanent
        | BuildStatus::FailedTimeout
        | BuildStatus::DependencyFailed
        | BuildStatus::Aborted => "build.failed",
        BuildStatus::FailedTransient => "build.failed_transient",
        BuildStatus::Substituted => "build.substituted",
        BuildStatus::Created => return None,
    })
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
            "gradient/my-project: Approval"
        );
    }

    #[test]
    fn evaluation_context_format() {
        assert_eq!(
            evaluation_check_context("my-project", None),
            "gradient/my-project: Evaluation"
        );
    }

    #[test]
    fn evaluation_context_format_with_custom_wildcard() {
        assert_eq!(
            evaluation_check_context("my-project", Some("packages.x86_64-linux.foo")),
            "gradient/my-project: Evaluation: packages.x86_64-linux.foo"
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
    fn suppresses_eval_failure_only_after_building() {
        assert!(suppress_evaluation_failure(&CiStatus::Failure, true));
        assert!(suppress_evaluation_failure(&CiStatus::Error, true));
        assert!(!suppress_evaluation_failure(&CiStatus::Failure, false));
        assert!(!suppress_evaluation_failure(&CiStatus::Success, true));
        assert!(!suppress_evaluation_failure(&CiStatus::Pending, true));
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
            ci_status_for_build(&BuildStatus::FailedPermanent),
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

    #[test]
    fn build_event_posts_live_progress() {
        // Queued/Building report before the terminal result so the per-build
        // check tracks progress, not just completion.
        assert_eq!(build_event_for_status(BuildStatus::Queued), Some("build.queued"));
        assert_eq!(build_event_for_status(BuildStatus::Building), Some("build.started"));
        assert_eq!(build_event_for_status(BuildStatus::Completed), Some("build.completed"));
        assert_eq!(build_event_for_status(BuildStatus::Substituted), Some("build.substituted"));
    }

    #[test]
    fn build_event_dependency_failure_and_abort_are_failures() {
        for s in [
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
            BuildStatus::DependencyFailed,
            BuildStatus::Aborted,
        ] {
            assert_eq!(build_event_for_status(s), Some("build.failed"));
            assert_eq!(
                crate::actions::forge_status_for_event(build_event_for_status(s).unwrap()),
                Some(CiStatus::Failure)
            );
        }
    }

    #[test]
    fn build_event_skips_created() {
        assert_eq!(build_event_for_status(BuildStatus::Created), None);
    }
}
