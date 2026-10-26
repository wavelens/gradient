/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_db::lookup::by_ids;
use gradient_db::maintenance::recovery::KeptAssignment;
use gradient_db::scheduling::assignment_record::{
    abandon_open_assignments, assignment_rows, eval_of_key, shared_build_of_key,
};
use gradient_entity::dispatched_job::Model as MDispatchedJob;
use gradient_types::*;
use tracing::warn;

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::{PendingJob, Reattached};

#[derive(Debug, Default)]
pub struct Reclaimed {
    pub reattached: Vec<Reattached>,
    pub unknown: Vec<(DispatchedJobId, Option<String>)>,
}

impl Scheduler {
    pub fn expect_reclaims(&self, kept: Vec<KeptAssignment>) {
        self.expected_reclaims.lock().extend(kept);
    }

    pub async fn reclaim_assignments(
        &self,
        worker_id: &str,
        listed: Vec<DispatchedJobId>,
    ) -> Reclaimed {
        let (mut reattached, unresolved) = self.reclaim_held(worker_id, &listed).await;
        let rows = self.listed_rows(&unresolved).await;
        let (rows, mut unknown) = classify_listed(worker_id, &unresolved, rows);
        let (restored, unbuildable) = self.jobs_of_rows(rows).await;
        unknown.extend(self.abandon_unbuildable(unbuildable).await);
        self.restore_reclaimed(worker_id, restored.clone(), &unknown)
            .await;
        self.requeue_unlisted(worker_id, &listed).await;

        reattached.extend(restored);
        Reclaimed {
            reattached,
            unknown,
        }
    }

    async fn reclaim_held(
        &self,
        worker_id: &str,
        listed: &[DispatchedJobId],
    ) -> (Vec<Reattached>, Vec<DispatchedJobId>) {
        let worker = worker_id.to_owned();
        let listed = listed.to_vec();
        self.call(|reply| SchedulerMsg::Reclaim {
            worker,
            listed: listed.clone(),
            reply,
        })
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, %worker_id, "reclaiming held jobs did not reach the scheduler");
            (Vec::new(), listed)
        })
    }

    async fn listed_rows(&self, ids: &[DispatchedJobId]) -> Vec<MDispatchedJob> {
        assignment_rows(&self.db.worker_db, ids)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "loading the listed assignment rows failed");
                Vec::new()
            })
    }

    async fn jobs_of_rows(
        &self,
        rows: Vec<MDispatchedJob>,
    ) -> (Vec<Reattached>, Vec<MDispatchedJob>) {
        let mut jobs = self.pending_jobs_of_keys(&rows).await;
        let (mut restored, mut unbuildable) = (Vec::new(), Vec::new());
        for row in rows {
            match row.job_id.as_ref().and_then(|key| jobs.remove(key)) {
                Some(job) => restored.push(Reattached::single(
                    row.job_id.clone().unwrap_or_default(),
                    row.id,
                    job,
                )),
                None => unbuildable.push(row),
            }
        }
        (restored, unbuildable)
    }

    async fn pending_jobs_of_keys(&self, rows: &[MDispatchedJob]) -> HashMap<String, PendingJob> {
        let keys: Vec<&str> = rows.iter().filter_map(|r| r.job_id.as_deref()).collect();
        let build_ids: Vec<DerivationBuildId> =
            keys.iter().filter_map(|k| shared_build_of_key(k)).collect();
        let eval_ids: Vec<EvaluationId> = keys.iter().filter_map(|k| eval_of_key(k)).collect();

        let builds = by_ids::<EDerivationBuild, _>(&self.db.worker_db, &build_ids)
            .await
            .map(|rows| {
                rows.into_values()
                    .filter(|b| !b.status.is_terminal())
                    .collect()
            })
            .unwrap_or_default();
        let evals = by_ids::<EEvaluation, _>(&self.db.worker_db, &eval_ids)
            .await
            .map(|rows| {
                rows.into_values()
                    .filter(|e| !e.status.is_terminal())
                    .collect()
            })
            .unwrap_or_default();

        let mut jobs = HashMap::new();
        match crate::loops::pending_build_jobs(self, builds).await {
            Ok(built) => jobs.extend(built.into_iter().map(|(k, j)| (k, PendingJob::Build(j)))),
            Err(e) => warn!(error = %e, "assembling reclaimed build jobs failed"),
        }
        match crate::loops::pending_eval_jobs(self, evals).await {
            Ok(evals) => jobs.extend(evals.into_iter().map(|(k, j)| (k, PendingJob::Eval(j)))),
            Err(e) => warn!(error = %e, "assembling reclaimed evaluation jobs failed"),
        }
        jobs
    }

    async fn abandon_unbuildable(
        &self,
        rows: Vec<MDispatchedJob>,
    ) -> Vec<(DispatchedJobId, Option<String>)> {
        let ids: Vec<DispatchedJobId> = rows.iter().map(|r| r.id).collect();
        if let Err(e) = abandon_open_assignments(&self.db.worker_db, &ids).await {
            warn!(error = %e, "closing the rows of reclaimed jobs that no longer run failed");
        }
        rows.into_iter().map(|r| (r.id, r.job_id)).collect()
    }

    async fn restore_reclaimed(
        &self,
        worker_id: &str,
        reattached: Vec<Reattached>,
        unknown: &[(DispatchedJobId, Option<String>)],
    ) {
        let worker = worker_id.to_owned();
        let dropped = unknown.iter().filter_map(|(_, key)| key.clone()).collect();
        if let Err(e) = self
            .call(|reply| SchedulerMsg::RestoreReclaimed {
                worker,
                reattached,
                dropped,
                reply,
            })
            .await
        {
            warn!(error = %e, %worker_id, "restoring reclaimed jobs did not reach the scheduler");
        }
    }

    async fn requeue_unlisted(&self, worker_id: &str, listed: &[DispatchedJobId]) {
        let worker = worker_id.to_owned();
        let listed: HashSet<DispatchedJobId> = listed.iter().copied().collect();
        let unlisted = self
            .call(|reply| SchedulerMsg::TakeUnlisted {
                worker,
                listed,
                reply,
            })
            .await
            .unwrap_or_default();
        self.requeue_held(unlisted).await;
    }
}

pub(crate) fn classify_listed(
    worker_id: &str,
    listed: &[DispatchedJobId],
    rows: Vec<MDispatchedJob>,
) -> (Vec<MDispatchedJob>, Vec<(DispatchedJobId, Option<String>)>) {
    let mut rows: HashMap<DispatchedJobId, MDispatchedJob> =
        rows.into_iter().map(|row| (row.id, row)).collect();
    let (mut reclaim, mut unknown) = (Vec::new(), Vec::new());
    for id in listed {
        match rows.remove(id) {
            Some(row)
                if row.finished_at.is_none()
                    && row.cluster_attempt.is_none()
                    && row.worker_id == worker_id =>
            {
                reclaim.push(row)
            }
            row => unknown.push((*id, row.and_then(|r| r.job_id))),
        }
    }
    (reclaim, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(worker: &str, open: bool) -> MDispatchedJob {
        MDispatchedJob {
            id: DispatchedJobId::now_v7(),
            worker_id: worker.into(),
            job_id: Some(format!("build:{}", DerivationBuildId::now_v7())),
            finished_at: (!open).then(now),
            ..Default::default()
        }
    }

    #[test]
    fn a_row_of_another_worker_or_a_closed_row_is_unknown() {
        let (mine, theirs, closed) = (row("w1", true), row("w2", true), row("w1", false));
        let missing = DispatchedJobId::now_v7();
        let listed = [mine.id, theirs.id, closed.id, missing];

        let (reclaim, unknown) = classify_listed(
            "w1",
            &listed,
            vec![closed.clone(), theirs.clone(), mine.clone()],
        );

        assert_eq!(reclaim, vec![mine]);
        assert_eq!(
            unknown,
            vec![
                (theirs.id, theirs.job_id),
                (closed.id, closed.job_id),
                (missing, None),
            ]
        );
    }
}
