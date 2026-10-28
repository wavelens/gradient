/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::eval_shards::{FailedShard, MAX_SHARD_ATTEMPTS, requeue_shards_of};
use gradient_graph::RecordBatch;
use tracing::{info, warn};

use crate::Scheduler;
use crate::cluster::{MemberOutcome, MemberReport};

impl Scheduler {
    pub(crate) async fn split_member_closed(&self, job_id: &str, report: &MemberReport) -> bool {
        if !self.cluster.book.split_member(job_id).await {
            return false;
        }

        let planned = match self.split_of(job_id).await {
            Ok(split) => split.is_some_and(|(split, _)| split.planned_at.is_some()),
            Err(e) => {
                warn!(%job_id, error = %e, "split evaluation lookup failed");
                false
            }
        };
        let lost = matches!(
            report.outcome(),
            MemberOutcome::Failed | MemberOutcome::Lost
        );
        if planned && lost {
            self.return_lost_shards(job_id).await;
        }

        planned
    }

    async fn return_lost_shards(&self, job_id: &str) {
        match requeue_shards_of(&self.db.worker_db, job_id).await {
            Ok(failed) => {
                info!(%job_id, failed = failed.len(), "shards of a lost split member returned to the queue");
                self.record_failed_shards(&failed).await;
            }
            Err(e) => {
                warn!(%job_id, error = %e, "returning the shards of a lost split member failed")
            }
        }
    }

    pub(crate) async fn record_failed_shards(&self, failed: &[FailedShard]) {
        let Some(evaluation) = failed.first().map(|shard| shard.evaluation) else {
            return;
        };

        let task = match gradient_db::evaluations::task_of(&self.db.worker_db, evaluation).await {
            Ok(task) => task,
            Err(e) => {
                warn!(%evaluation, error = %e, "task lookup for failed shards failed");
                None
            }
        };
        let batch = RecordBatch {
            evaluation,
            task,
            derivations: Vec::new(),
            warnings: Vec::new(),
            errors: failed.iter().map(shard_error).collect(),
            continuation: None,
        };
        if let Err(e) = self.graph.record(batch).await {
            warn!(%evaluation, error = %e, "recording the errors of failed shards failed");
        }
    }
}

fn shard_error(shard: &FailedShard) -> String {
    let attributes = match &shard.children {
        Some(children) => format!("{} ({})", shard.pattern, children.join(", ")),
        None => shard.pattern.clone(),
    };

    format!("{attributes}: evaluation lost its worker {MAX_SHARD_ATTEMPTS} times")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{AttemptMember, AttemptState, PendingCluster};
    use crate::jobs::{PendingJob, eval_job_key};
    use crate::test_jobs::eval_job;
    use gradient_entity::eval_shard::{EvalShardState, Model as MEvalShard};
    use gradient_entity::eval_split::Model as MEvalSplit;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use gradient_types::ids::{
        ClusterAttemptId, ClusterJobId, DispatchedJobId, EvalShardId, EvaluationId, ProjectId,
    };
    use std::time::Instant;

    fn member(job_id: String, primary: bool) -> AttemptMember {
        AttemptMember {
            worker: format!("w-{job_id}"),
            job_id,
            assignment_id: DispatchedJobId::now_v7(),
            role: "r".into(),
            index: 0,
            primary,
            accepted: true,
            report: None,
            settled: false,
        }
    }

    fn attempt(cluster: ClusterJobId, members: Vec<AttemptMember>) -> AttemptState {
        AttemptState {
            cluster,
            parked: PendingCluster {
                id: cluster,
                same_zone: false,
                queued_at: gradient_types::now(),
                startable_since: None,
                expected: 0,
                members: Vec::new(),
                not_before: None,
            },
            members,
            roster: Vec::new(),
            deadline: Instant::now(),
            started: true,
            all_accepted: true,
            verdict: None,
            resolving: false,
            resolution: None,
            resolved_at: None,
            split: true,
            planned: false,
        }
    }

    #[tokio::test]
    async fn a_lost_companion_requeues_its_shard_and_the_split_continues() {
        let evaluation = EvaluationId::now_v7();
        let cluster = ClusterJobId::now_v7();
        let (primary, companion) = (eval_job_key(evaluation, 0), eval_job_key(evaluation, 1));
        let running = MEvalShard {
            id: EvalShardId::now_v7(),
            evaluation,
            seq: 4,
            pattern: "packages.*".into(),
            children: Some(vec!["hello".into()]),
            state: EvalShardState::Running,
            taken_job: Some(companion.clone()),
            created_at: gradient_types::now(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![MEvalSplit {
                evaluation,
                cluster_job: cluster,
                planned_at: Some(gradient_types::now()),
                ..Default::default()
            }]])
            .append_query_results([vec![running]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let scheduler = Scheduler::new(TestState::new().worker_db(db).build());
        let attempt = ClusterAttemptId::now_v7();
        scheduler
            .cluster
            .book
            .open(
                attempt,
                self::attempt(
                    cluster,
                    vec![
                        member(primary.clone(), true),
                        member(companion.clone(), false),
                    ],
                ),
            )
            .await;

        let lost = MemberReport::Lost {
            job: PendingJob::Eval(eval_job(ProjectId::now_v7())),
        };
        scheduler
            .on_cluster_member_closed(&companion, lost)
            .await
            .expect("closed");

        assert!(scheduler.cluster.book.pending_verdicts().await.is_empty());
        assert_eq!(
            scheduler.cluster.book.attempt_of(&primary).await,
            Some(attempt)
        );
    }
}
