/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_db::state_machine::BuildStateMachine;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use gradient_wire::types::ImportOutcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedBuild {
    pub derivation_build: DerivationBuildId,
    pub build_id: BuildJobId,
    pub drv_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportWait {
    pub worker: String,
    pub job_id: String,
    pub request_id: String,
    pub evaluation: EvaluationId,
    pub builds: Vec<ImportedBuild>,
    pub since: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportAnswer {
    pub worker: String,
    pub job_id: String,
    pub request_id: String,
    pub outcome: ImportOutcome,
}

impl ImportAnswer {
    fn to(wait: ImportWait, outcome: ImportOutcome) -> Self {
        Self {
            worker: wait.worker,
            job_id: wait.job_id,
            request_id: wait.request_id,
            outcome,
        }
    }
}

#[derive(Debug, Default)]
pub struct ImportWaits {
    waits: Vec<ImportWait>,
}

impl ImportWaits {
    pub fn is_empty(&self) -> bool {
        self.waits.is_empty()
    }

    pub fn register(&mut self, wait: ImportWait) {
        self.waits.push(wait);
    }

    pub fn waited_builds(&self) -> Vec<DerivationBuildId> {
        self.waits
            .iter()
            .flat_map(|w| w.builds.iter().map(|b| b.derivation_build))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn waiting_evaluations(&self) -> Vec<EvaluationId> {
        self.waits
            .iter()
            .map(|w| w.evaluation)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn overdue_builds(&self, before: chrono::NaiveDateTime) -> Vec<DerivationBuildId> {
        self.waits
            .iter()
            .filter(|w| w.since <= before)
            .flat_map(|w| w.builds.iter().map(|b| b.derivation_build))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn settle(
        &mut self,
        statuses: &HashMap<DerivationBuildId, BuildStatus>,
    ) -> Vec<ImportAnswer> {
        self.take_answered(|wait| outcome(wait, statuses))
    }

    pub fn answer_unbuildable(
        &mut self,
        unbuildable: &HashMap<DerivationBuildId, String>,
        waiting_for_workers: &HashSet<EvaluationId>,
        before: chrono::NaiveDateTime,
    ) -> Vec<ImportAnswer> {
        self.take_answered(|wait| {
            if wait.since > before || waiting_for_workers.contains(&wait.evaluation) {
                return None;
            }

            wait.builds.iter().find_map(|build| {
                unbuildable
                    .get(&build.derivation_build)
                    .map(|reason| ImportOutcome::Failed {
                        drv_path: build.drv_path.clone(),
                        build_id: build.build_id.to_string(),
                        status: reason.clone(),
                    })
            })
        })
    }

    fn take_answered(
        &mut self,
        decide: impl Fn(&ImportWait) -> Option<ImportOutcome>,
    ) -> Vec<ImportAnswer> {
        let (answered, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waits)
            .into_iter()
            .map(|wait| (decide(&wait), wait))
            .partition(|(outcome, _)| outcome.is_some());
        self.waits = waiting.into_iter().map(|(_, wait)| wait).collect();

        answered
            .into_iter()
            .filter_map(|(outcome, wait)| outcome.map(|o| ImportAnswer::to(wait, o)))
            .collect()
    }

    pub fn drop_evaluations(&mut self, evaluations: &HashSet<EvaluationId>) -> usize {
        let before = self.waits.len();
        self.waits.retain(|w| !evaluations.contains(&w.evaluation));
        before - self.waits.len()
    }
}

fn outcome(
    wait: &ImportWait,
    statuses: &HashMap<DerivationBuildId, BuildStatus>,
) -> Option<ImportOutcome> {
    let mut failed = None;
    for build in &wait.builds {
        let status = *statuses.get(&build.derivation_build)?;
        if !BuildStateMachine::is_terminal(&status) {
            return None;
        }

        if failed.is_none() && !status.is_terminal_success() {
            failed = Some(ImportOutcome::Failed {
                drv_path: build.drv_path.clone(),
                build_id: build.build_id.to_string(),
                status: format!("{status:?}"),
            });
        }
    }

    Some(failed.unwrap_or(ImportOutcome::Completed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imported(drv_path: &str) -> ImportedBuild {
        ImportedBuild {
            derivation_build: DerivationBuildId::now_v7(),
            build_id: BuildJobId::now_v7(),
            drv_path: drv_path.to_owned(),
        }
    }

    fn wait(evaluation: EvaluationId, builds: Vec<ImportedBuild>) -> ImportWait {
        ImportWait {
            worker: "w1".into(),
            job_id: "eval:x".into(),
            request_id: "r1".into(),
            evaluation,
            builds,
            since: gradient_types::now(),
        }
    }

    #[test]
    fn the_last_terminal_build_settles_the_request_with_the_first_failure() {
        let (a, b, c) = (imported("a.drv"), imported("b.drv"), imported("c.drv"));
        let mut waits = ImportWaits::default();
        waits.register(wait(
            EvaluationId::now_v7(),
            vec![a.clone(), b.clone(), c.clone()],
        ));

        let mut statuses = HashMap::from([
            (a.derivation_build, BuildStatus::Completed),
            (b.derivation_build, BuildStatus::DependencyFailed),
            (c.derivation_build, BuildStatus::Building),
        ]);
        assert!(waits.settle(&statuses).is_empty(), "c is still building");

        statuses.insert(c.derivation_build, BuildStatus::FailedPermanent);
        let answers = waits.settle(&statuses);
        assert_eq!(answers.len(), 1);
        assert_eq!(
            answers[0].outcome,
            ImportOutcome::Failed {
                drv_path: "b.drv".into(),
                build_id: b.build_id.to_string(),
                status: "DependencyFailed".into(),
            }
        );
        assert!(waits.is_empty());
        assert!(waits.settle(&statuses).is_empty(), "a request answers once");
    }

    #[test]
    fn a_substituted_import_completes_and_a_retried_import_keeps_waiting() {
        let (a, b) = (imported("a.drv"), imported("b.drv"));
        let mut waits = ImportWaits::default();
        waits.register(wait(EvaluationId::now_v7(), vec![a.clone(), b.clone()]));

        let mut statuses = HashMap::from([
            (a.derivation_build, BuildStatus::Substituted),
            (b.derivation_build, BuildStatus::FailedTransient),
        ]);
        assert!(waits.settle(&statuses).is_empty());

        statuses.insert(b.derivation_build, BuildStatus::Skipped);
        assert!(
            waits.settle(&statuses).is_empty(),
            "a skipped build can come back"
        );

        statuses.insert(b.derivation_build, BuildStatus::Completed);
        let answers = waits.settle(&statuses);
        assert_eq!(answers[0].outcome, ImportOutcome::Completed);
        assert_eq!(
            (answers[0].worker.as_str(), answers[0].request_id.as_str()),
            ("w1", "r1")
        );
    }

    #[test]
    fn an_aborted_evaluation_drops_its_waits() {
        let (aborted, live) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let (a, b) = (imported("a.drv"), imported("b.drv"));
        let mut waits = ImportWaits::default();
        waits.register(wait(aborted, vec![a.clone()]));
        waits.register(wait(live, vec![b.clone()]));

        assert_eq!(waits.drop_evaluations(&HashSet::from([aborted])), 1);

        let answers = waits.settle(&HashMap::from([
            (a.derivation_build, BuildStatus::Aborted),
            (b.derivation_build, BuildStatus::Completed),
        ]));
        assert_eq!(answers.len(), 1, "no answer reaches the aborted job");
        assert_eq!(waits.waiting_evaluations(), Vec::<EvaluationId>::new());
    }

    #[test]
    fn an_overdue_import_no_worker_can_build_fails_with_the_reason() {
        let (a, b) = (imported("a.drv"), imported("b.drv"));
        let (patient, impatient) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let mut waits = ImportWaits::default();
        let fresh = wait(impatient, vec![b.clone()]);
        let old = |evaluation| ImportWait {
            since: fresh.since - chrono::Duration::minutes(10),
            ..wait(evaluation, vec![a.clone(), b.clone()])
        };
        waits.register(old(patient));
        waits.register(old(impatient));
        waits.register(fresh.clone());

        let reason = "no connected worker provides aarch64-darwin".to_owned();
        let answers = waits.answer_unbuildable(
            &HashMap::from([(b.derivation_build, reason.clone())]),
            &HashSet::from([patient]),
            fresh.since - chrono::Duration::minutes(5),
        );

        assert_eq!(
            answers.len(),
            1,
            "a task waiting for workers keeps waiting, a fresh wait too"
        );
        assert_eq!(
            answers[0].outcome,
            ImportOutcome::Failed {
                drv_path: "b.drv".into(),
                build_id: b.build_id.to_string(),
                status: reason,
            }
        );
        assert_eq!(waits.waiting_evaluations().len(), 2);
    }
}
