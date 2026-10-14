/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_ci::{AbortKind, abort_evaluation};
use gradient_db::evaluations::branch::active_on_branch;
use gradient_db::lookup::by_id;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::Transition;
use gradient_types::branch_key::NO_BRANCH;
use gradient_types::triggers::ConcurrencyPolicy;
use gradient_types::*;
use tracing::{info, warn};

use crate::Scheduler;

pub(crate) fn superseded_by(
    newer: &MEvaluation,
    policy: ConcurrencyPolicy,
    active: &[MEvaluation],
) -> Vec<EvaluationId> {
    if policy != ConcurrencyPolicy::Auto
        || newer.status != EvaluationStatus::Building
        || newer.branch == NO_BRANCH
    {
        return Vec::new();
    }

    active
        .iter()
        .filter(|older| older.task == newer.task && older.branch == newer.branch)
        .filter(|older| older.status.is_active())
        .filter(|older| (older.created_at, older.id) < (newer.created_at, newer.id))
        .map(|older| older.id)
        .collect()
}

impl Scheduler {
    pub(crate) async fn complete_eval_stream(
        &self,
        evaluation: EvaluationId,
    ) -> anyhow::Result<()> {
        self.graph
            .transition(Transition::EvalStreamCompleted { evaluation })
            .await?;
        self.abort_superseded_evaluations(evaluation).await;
        Ok(())
    }

    async fn abort_superseded_evaluations(&self, evaluation: EvaluationId) {
        if let Err(e) = self.try_abort_superseded_evaluations(evaluation).await {
            warn!(error = %e, evaluation_id = %evaluation, "aborting the evaluations an evaluation supersedes failed");
        }
    }

    async fn try_abort_superseded_evaluations(
        &self,
        evaluation: EvaluationId,
    ) -> Result<(), sea_orm::DbErr> {
        let db = &self.db.worker_db;
        let Some(newer) = by_id::<EEvaluation, _>(db, evaluation).await? else {
            return Ok(());
        };
        let Some(task_id) = newer.task else {
            return Ok(());
        };
        let Some(task) = by_id::<ETask, _>(db, task_id).await? else {
            return Ok(());
        };

        let active = active_on_branch(db, task_id, &newer.branch).await?;
        for older in superseded_by(&newer, task.concurrency, &active) {
            if abort_evaluation(db, older, AbortKind::Hard).await? {
                info!(evaluation_id = %older, newer = %evaluation, "aborting an evaluation a newer evaluation of its branch supersedes");
                self.stop_aborted_evaluation(older, true).await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = "refs/heads/main";

    fn evaluation(task: TaskId, branch: &str, status: EvaluationStatus, age: i64) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(task),
            branch: branch.to_owned(),
            status,
            created_at: gradient_types::now() - chrono::Duration::seconds(age),
            ..Default::default()
        }
    }

    #[test]
    fn a_building_evaluation_supersedes_the_older_active_ones_of_its_branch() {
        let task = TaskId::now_v7();
        let newer = evaluation(task, MAIN, EvaluationStatus::Building, 10);
        let older = evaluation(task, MAIN, EvaluationStatus::EvaluatingDerivation, 60);
        let older_building = evaluation(task, MAIN, EvaluationStatus::Building, 30);
        let finished = evaluation(task, MAIN, EvaluationStatus::Completed, 90);
        let younger = evaluation(task, MAIN, EvaluationStatus::Queued, 1);
        let other_branch = evaluation(task, "pull/1", EvaluationStatus::Building, 60);
        let other_task = evaluation(TaskId::now_v7(), MAIN, EvaluationStatus::Building, 60);
        let candidates = [
            newer.clone(),
            older.clone(),
            older_building.clone(),
            finished,
            younger,
            other_branch,
            other_task,
        ];

        assert_eq!(
            superseded_by(&newer, ConcurrencyPolicy::Auto, &candidates),
            vec![older.id, older_building.id]
        );
    }

    #[test]
    fn only_auto_on_a_building_evaluation_of_a_branch_supersedes() {
        let task = TaskId::now_v7();
        let older = evaluation(task, MAIN, EvaluationStatus::Building, 60);
        let building = evaluation(task, MAIN, EvaluationStatus::Building, 10);
        let evaluating = evaluation(task, MAIN, EvaluationStatus::EvaluatingDerivation, 10);
        let out_of_band = MEvaluation {
            branch: NO_BRANCH.to_owned(),
            ..building.clone()
        };
        let older_out_of_band = MEvaluation {
            branch: NO_BRANCH.to_owned(),
            ..older.clone()
        };

        for policy in [
            ConcurrencyPolicy::HardAbort,
            ConcurrencyPolicy::SoftAbort,
            ConcurrencyPolicy::All,
            ConcurrencyPolicy::Skip,
        ] {
            assert_eq!(
                superseded_by(&building, policy, std::slice::from_ref(&older)),
                vec![],
                "{policy:?}"
            );
        }
        assert_eq!(
            superseded_by(&evaluating, ConcurrencyPolicy::Auto, &[older]),
            vec![]
        );
        assert_eq!(
            superseded_by(&out_of_band, ConcurrencyPolicy::Auto, &[older_out_of_band]),
            vec![]
        );
    }
}
