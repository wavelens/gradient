/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::{MDerivationBuild, MEvaluation};

pub(crate) fn is_build_continued_after_aborted_evaluation_finished(
    evaluation: &MEvaluation,
    build: &MDerivationBuild,
) -> bool {
    evaluation.status == EvaluationStatus::Aborted
        && build.status != BuildStatus::Aborted
        && (!build.status.is_terminal()
            || evaluation
                .finished_at
                .is_some_and(|finished| build.updated_at > finished))
}

pub(crate) fn shared_build_status_for_evaluation(
    evaluation: &MEvaluation,
    build: &MDerivationBuild,
) -> BuildStatus {
    if is_build_continued_after_aborted_evaluation_finished(evaluation, build) {
        BuildStatus::Aborted
    } else {
        build.status.for_api()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDateTime};

    fn at(minute: i64) -> NaiveDateTime {
        gradient_fixtures::test_date() + Duration::minutes(minute)
    }

    fn aborted_at(minute: i64) -> MEvaluation {
        MEvaluation {
            status: EvaluationStatus::Aborted,
            finished_at: Some(at(minute)),
            ..Default::default()
        }
    }

    fn build(status: BuildStatus, updated_minute: i64) -> MDerivationBuild {
        MDerivationBuild {
            status,
            updated_at: at(updated_minute),
            ..Default::default()
        }
    }

    #[test]
    fn a_build_still_running_after_the_abort_counts_as_aborted() {
        let status =
            shared_build_status_for_evaluation(&aborted_at(10), &build(BuildStatus::Building, 5));
        assert_eq!(status, BuildStatus::Aborted);
    }

    #[test]
    fn a_build_finished_after_the_abort_counts_as_aborted() {
        let status =
            shared_build_status_for_evaluation(&aborted_at(10), &build(BuildStatus::Completed, 20));
        assert_eq!(status, BuildStatus::Aborted);
    }

    #[test]
    fn a_build_finished_before_the_abort_keeps_its_status() {
        let status =
            shared_build_status_for_evaluation(&aborted_at(10), &build(BuildStatus::Completed, 5));
        assert_eq!(status, BuildStatus::Completed);
    }

    #[test]
    fn a_running_evaluation_sees_the_shared_status() {
        let evaluation = MEvaluation {
            status: EvaluationStatus::Building,
            ..Default::default()
        };
        let status =
            shared_build_status_for_evaluation(&evaluation, &build(BuildStatus::Created, 5));
        assert_eq!(status, BuildStatus::Queued);
    }
}
