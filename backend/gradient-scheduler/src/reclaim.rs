/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_db::exec_jobs::dispatch::started as started_exec_jobs;
use gradient_db::lookup::by_ids;
use gradient_db::maintenance::recovery::KeptAssignment;
use gradient_db::scheduling::assignment_record::{
    abandon_open_assignments, assignment_rows, eval_of_key, exec_job_of_key,
    open_single_assignments_of_disconnected_workers, open_single_assignments_of_worker,
    shared_build_of_key,
};
use gradient_entity::dispatched_job::Model as MDispatchedJob;
use gradient_graph::OrphanCause;
use gradient_types::*;
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::{AwaitingReclaim, HeldReclaim, PendingJob, Reattached};

#[derive(Debug, Default)]
pub struct Reclaimed {
    pub reattached: Vec<Reattached>,
    pub unknown: Vec<(DispatchedJobId, Option<String>)>,
}

#[derive(Default)]
struct RowJobs {
    jobs: HashMap<String, PendingJob>,
    live: HashSet<String>,
}

#[derive(Default)]
struct SettledRows {
    restored: Vec<Reattached>,
    live: Vec<MDispatchedJob>,
    finished: Vec<MDispatchedJob>,
}

impl Scheduler {
    pub fn expect_reclaims(&self, kept: Vec<KeptAssignment>) {
        self.expected_reclaims.lock().extend(kept);
    }

    pub(crate) async fn held_for_new_core(&self) -> Vec<KeptAssignment> {
        let mut kept = std::mem::take(&mut *self.expected_reclaims.lock());
        let respawn = self.core.borrow().is_some();
        if !respawn {
            return kept;
        }
        let rows = match open_single_assignments_of_disconnected_workers(&self.db.worker_db).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "reading the held rows for the new scheduler core failed");
                return kept;
            }
        };
        let known: HashSet<DispatchedJobId> = kept.iter().map(|k| k.assignment_id).collect();
        kept.extend(
            rows.into_iter()
                .filter(|(id, _, _)| !known.contains(id))
                .map(|(assignment_id, job_key, worker_id)| KeptAssignment {
                    assignment_id,
                    worker_id,
                    job_key,
                }),
        );
        kept
    }

    pub async fn reclaim_assignments(
        &self,
        worker_id: &str,
        listed: Vec<DispatchedJobId>,
    ) -> Reclaimed {
        let listed = without_repeats(listed);
        let held = self.reclaim_held(worker_id, &listed).await;
        let rows = match assignment_rows(&self.db.worker_db, &held.unresolved).await {
            Ok(rows) => rows,
            Err(e) => return self.keep_holding(worker_id, held, e.into()).await,
        };
        let (rows, mut unknown) = classify_listed(worker_id, &held.unresolved, rows);
        let row_jobs = match self.pending_jobs_of_rows(&rows).await {
            Ok(row_jobs) => row_jobs,
            Err(e) => return self.keep_holding(worker_id, held, e).await,
        };
        let settled = settle_rows(rows, row_jobs);
        unknown.extend(self.abandon_finished(settled.finished).await);
        unknown.extend(self.requeue_live(settled.live).await);
        let refused = self
            .restore_reclaimed(worker_id, settled.restored.clone(), Vec::new())
            .await;
        let refused_ids: HashSet<DispatchedJobId> =
            refused.iter().map(|job| job.assignment_id).collect();
        unknown.extend(
            refused
                .into_iter()
                .map(|job| (job.assignment_id, Some(job.job_id))),
        );
        self.requeue_unlisted(worker_id, &listed).await;
        self.requeue_untracked_rows(worker_id, &listed).await;

        let mut reattached = held.reattached;
        reattached.extend(
            settled
                .restored
                .into_iter()
                .filter(|job| !refused_ids.contains(&job.assignment_id)),
        );
        Reclaimed {
            reattached,
            unknown,
        }
    }

    async fn reclaim_held(&self, worker_id: &str, listed: &[DispatchedJobId]) -> HeldReclaim {
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
            HeldReclaim {
                unresolved: listed,
                ..Default::default()
            }
        })
    }

    async fn keep_holding(
        &self,
        worker_id: &str,
        held: HeldReclaim,
        error: anyhow::Error,
    ) -> Reclaimed {
        warn!(error = %error, %worker_id, "reclaim left the listed rows to the reclaim grace");
        self.restore_reclaimed(worker_id, Vec::new(), held.seeded)
            .await;
        Reclaimed {
            reattached: held.reattached,
            unknown: Vec::new(),
        }
    }

    async fn pending_jobs_of_rows(&self, rows: &[MDispatchedJob]) -> anyhow::Result<RowJobs> {
        let keys: Vec<&str> = rows.iter().filter_map(|r| r.job_id.as_deref()).collect();
        let build_ids: Vec<DerivationBuildId> =
            keys.iter().filter_map(|k| shared_build_of_key(k)).collect();
        let eval_ids: Vec<EvaluationId> = keys.iter().filter_map(|k| eval_of_key(k)).collect();
        let exec_ids: Vec<ExecJobId> = keys.iter().filter_map(|k| exec_job_of_key(k)).collect();

        let builds: Vec<MDerivationBuild> =
            by_ids::<EDerivationBuild, _>(&self.db.worker_db, &build_ids)
                .await?
                .into_values()
                .filter(|b| !b.status.is_terminal())
                .collect();
        let evals: Vec<MEvaluation> = by_ids::<EEvaluation, _>(&self.db.worker_db, &eval_ids)
            .await?
            .into_values()
            .filter(|e| !e.status.is_terminal())
            .collect();

        let mut row_jobs = RowJobs::default();
        row_jobs
            .live
            .extend(builds.iter().map(|b| crate::jobs::build_job_key(b.id)));
        row_jobs
            .live
            .extend(evals.iter().map(|e| crate::jobs::eval_job_key(e.id, 0)));
        let built = crate::loops::pending_build_jobs(self, builds).await?;
        let evaluated = crate::loops::pending_eval_jobs(self, evals).await?;
        row_jobs.jobs.extend(
            built
                .into_iter()
                .map(|(key, job)| (key, PendingJob::Build(job))),
        );
        row_jobs.jobs.extend(
            evaluated
                .into_iter()
                .map(|(key, job)| (key, PendingJob::Eval(job))),
        );
        let api_url = &self.db.config.server.serve_url;
        for row in started_exec_jobs(&self.db.worker_db, &exec_ids).await? {
            let key = crate::jobs::exec_job_key(row.job.id);
            row_jobs.live.insert(key.clone());
            if let Some(job) =
                crate::loops::pending_exec_job(&row, api_url).filter(|job| job.assignment.is_some())
            {
                row_jobs.jobs.insert(key, PendingJob::Exec(job));
            }
        }
        Ok(row_jobs)
    }

    async fn abandon_finished(
        &self,
        rows: Vec<MDispatchedJob>,
    ) -> Vec<(DispatchedJobId, Option<String>)> {
        let ids: Vec<DispatchedJobId> = rows.iter().map(|r| r.id).collect();
        if let Err(e) = abandon_open_assignments(&self.db.worker_db, &ids).await {
            warn!(error = %e, "closing the rows of reclaimed jobs that no longer run failed");
        }
        rows.into_iter().map(|r| (r.id, r.job_id)).collect()
    }

    async fn requeue_live(
        &self,
        rows: Vec<MDispatchedJob>,
    ) -> Vec<(DispatchedJobId, Option<String>)> {
        let keys: Vec<String> = rows.iter().filter_map(|r| r.job_id.clone()).collect();
        if !keys.is_empty() {
            warn!(
                jobs = keys.len(),
                "re-queuing reclaimed jobs the scheduler cannot assemble"
            );
            self.requeue_keys(&keys).await;
        }
        rows.into_iter().map(|r| (r.id, r.job_id)).collect()
    }

    async fn restore_reclaimed(
        &self,
        worker_id: &str,
        reattached: Vec<Reattached>,
        hold_again: Vec<(String, AwaitingReclaim)>,
    ) -> Vec<Reattached> {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::RestoreReclaimed {
            worker,
            reattached,
            hold_again,
            reply,
        })
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, %worker_id, "restoring reclaimed jobs did not reach the scheduler");
            Vec::new()
        })
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

    async fn requeue_untracked_rows(&self, worker_id: &str, listed: &[DispatchedJobId]) {
        let rows = match open_single_assignments_of_worker(&self.db.worker_db, worker_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, %worker_id, "reading the worker's open rows failed");
                return;
            }
        };
        let keys: Vec<String> = rows
            .into_iter()
            .filter(|(id, _)| !listed.contains(id))
            .map(|(_, key)| key)
            .collect();
        if keys.is_empty() {
            return;
        }
        let untracked = self.untracked(keys).await;
        if !untracked.is_empty() {
            info!(%worker_id, jobs = untracked.len(), "re-queuing open rows the worker no longer runs");
            self.requeue_keys(&untracked).await;
        }
    }

    async fn requeue_keys(&self, keys: &[String]) {
        crate::build::requeue_orphaned_keys(
            &self.db,
            &self.graph,
            keys,
            &HashMap::new(),
            OrphanCause::WorkerLost,
        )
        .await;
        self.build_kick.kick();
    }
}

fn without_repeats(listed: Vec<DispatchedJobId>) -> Vec<DispatchedJobId> {
    let mut seen = HashSet::new();
    listed.into_iter().filter(|id| seen.insert(*id)).collect()
}

fn settle_rows(rows: Vec<MDispatchedJob>, mut row_jobs: RowJobs) -> SettledRows {
    let mut settled = SettledRows::default();
    for row in rows {
        let key = row.job_id.clone().unwrap_or_default();
        match row_jobs.jobs.remove(&key) {
            Some(job) => settled.restored.push(Reattached::single(key, row.id, job)),
            None if row_jobs.live.contains(&key) => settled.live.push(row),
            None => settled.finished.push(row),
        }
    }
    settled
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
