/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;

/// The grace is keeping a server restart or worker redeploy from aborting everything in flight
/// before the pool is back.
const UNBUILDABLE_GRACE_SECS: i64 = 300;

pub(crate) struct Unbuildable {
    pub evaluation: MEvaluation,
    pub unmet: Vec<UnmetRequirement>,
}

pub(crate) fn unbuildable(
    eval: &MEvaluation,
    current: Option<&WaitingReason>,
    next: Option<&WaitingReason>,
    waits: bool,
    now: chrono::NaiveDateTime,
) -> Option<Vec<UnmetRequirement>> {
    if waits
        || eval.status != EvaluationStatus::Waiting
        || (now - eval.updated_at).num_seconds() < UNBUILDABLE_GRACE_SECS
    {
        return None;
    }

    match (current, next) {
        (
            Some(WaitingReason::Workers { unmet: parked, .. }),
            Some(WaitingReason::Workers { unmet, .. }),
        ) if !unmet.is_empty() && parked == unmet => Some(unmet.clone()),
        _ => None,
    }
}

pub(crate) fn unbuildable_warning(unmet: &[UnmetRequirement]) -> String {
    let systems: Vec<String> = unmet
        .iter()
        .map(|u| {
            let features = if u.required_features.is_empty() {
                String::new()
            } else {
                format!(" with features {}", u.required_features.join(", "))
            };
            format!("{}{features} ({} builds)", u.architecture, u.build_count)
        })
        .collect();

    format!(
        "Aborted: no connected worker provides {}. Enable waiting for workers on the task to wait instead.",
        systems.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn unmet(architecture: &str, features: &[&str], build_count: u32) -> UnmetRequirement {
        UnmetRequirement {
            architecture: architecture.into(),
            required_features: features.iter().map(|f| f.to_string()).collect(),
            build_count,
        }
    }

    fn workers(unmet: Vec<UnmetRequirement>) -> WaitingReason {
        WaitingReason::workers(unmet, 0, vec![])
    }

    fn parked(age_secs: i64) -> (MEvaluation, chrono::NaiveDateTime) {
        let now = gradient_types::now();
        let eval = MEvaluation {
            status: EvaluationStatus::Waiting,
            updated_at: now - Duration::seconds(age_secs),
            ..Default::default()
        };
        (eval, now)
    }

    #[test]
    fn a_task_not_waiting_aborts_a_stable_park_past_the_grace() {
        let (eval, now) = parked(UNBUILDABLE_GRACE_SECS);
        let reason = workers(vec![unmet("aarch64-linux", &[], 142)]);

        assert_eq!(
            unbuildable(&eval, Some(&reason), Some(&reason), false, now),
            Some(vec![unmet("aarch64-linux", &[], 142)])
        );
    }

    #[test]
    fn a_task_waiting_for_workers_keeps_its_park() {
        let (eval, now) = parked(UNBUILDABLE_GRACE_SECS * 10);
        let reason = workers(vec![unmet("aarch64-linux", &[], 1)]);

        assert_eq!(
            unbuildable(&eval, Some(&reason), Some(&reason), true, now),
            None
        );
    }

    #[test]
    fn a_fresh_park_waits_out_the_grace() {
        let (eval, now) = parked(UNBUILDABLE_GRACE_SECS - 1);
        let reason = workers(vec![unmet("aarch64-linux", &[], 1)]);

        assert_eq!(
            unbuildable(&eval, Some(&reason), Some(&reason), false, now),
            None
        );
    }

    #[test]
    fn a_park_whose_unmet_systems_moved_is_not_aborted() {
        let (eval, now) = parked(UNBUILDABLE_GRACE_SECS);
        let before = workers(vec![unmet("aarch64-linux", &[], 2)]);
        let after = workers(vec![unmet("aarch64-linux", &[], 1)]);

        assert_eq!(
            unbuildable(&eval, Some(&before), Some(&after), false, now),
            None
        );
    }

    #[test]
    fn only_a_workers_park_with_unmet_systems_is_aborted() {
        let (eval, now) = parked(UNBUILDABLE_GRACE_SECS);
        let stuck = workers(vec![]);
        let graph_stuck = WaitingReason::graph_stuck(3);

        assert_eq!(
            unbuildable(&eval, Some(&stuck), Some(&stuck), false, now),
            None
        );
        assert_eq!(
            unbuildable(&eval, Some(&graph_stuck), Some(&graph_stuck), false, now),
            None
        );
        assert_eq!(unbuildable(&eval, None, None, false, now), None);
    }

    #[test]
    fn a_building_evaluation_is_not_aborted() {
        let (mut eval, now) = parked(UNBUILDABLE_GRACE_SECS);
        eval.status = EvaluationStatus::Building;
        let reason = workers(vec![unmet("aarch64-linux", &[], 1)]);

        assert_eq!(
            unbuildable(&eval, Some(&reason), Some(&reason), false, now),
            None
        );
    }

    #[test]
    fn the_warning_names_every_missing_architecture_and_feature() {
        let warning = unbuildable_warning(&[
            unmet("aarch64-linux", &[], 142),
            unmet("x86_64-linux", &["kvm", "nixos-test"], 3),
        ]);

        assert!(warning.contains("aarch64-linux (142 builds)"));
        assert!(warning.contains("x86_64-linux with features kvm, nixos-test (3 builds)"));
    }
}
