/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job queue: enqueue, candidate listing, and diagnostics.

use crate::Scheduler;
use crate::actor::{Offer, SchedulerMsg};
use crate::jobs::{PendingBuildJob, PendingEvalJob, PendingJob};
use crate::worker_pool::WorkerInfo;
use gradient_types::proto::{GradientCapabilities, JobCandidate};

impl Scheduler {
    pub async fn enqueue_eval_job(
        &self,
        job_id: String,
        job: PendingEvalJob,
    ) -> anyhow::Result<()> {
        self.call(|reply| SchedulerMsg::Enqueue {
            job_id,
            job: PendingJob::Eval(job),
            reply,
        })
        .await
    }

    pub async fn enqueue_build_job(
        &self,
        job_id: String,
        job: PendingBuildJob,
    ) -> anyhow::Result<()> {
        self.call(|reply| SchedulerMsg::Enqueue {
            job_id,
            job: PendingJob::Build(job),
            reply,
        })
        .await
    }

    /// Wake the build dispatcher now instead of waiting for its 5s tick.
    pub(crate) fn kick_dispatch(&self) {
        self.kick_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(actor) = self.build_dispatch.load_full() {
            let _ = actor.send_message(crate::dispatch::BuildMsg::Kick);
        }
    }

    /// Every pending candidate visible to the worker (`RequestJobList`,
    /// `RequestAllCandidates`); all of them are marked sent.
    pub async fn get_job_candidates(&self, worker_id: &str) -> Vec<JobCandidate> {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::Candidates {
            worker,
            only_new: false,
            reply,
        })
        .await
        .map(|o| o.candidates)
        .unwrap_or_default()
    }

    /// Only the candidates not yet sent to the worker, with the offer
    /// generation they answer so the session can skip stale `Offers` signals.
    pub async fn get_new_job_candidates(&self, worker_id: &str) -> Offer {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::Candidates {
            worker,
            only_new: true,
            reply,
        })
        .await
        .unwrap_or_default()
    }

    pub async fn worker_gradient_caps(&self, worker_id: &str) -> Option<GradientCapabilities> {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::WorkerCaps { worker, reply })
            .await
            .ok()
            .flatten()
    }

    pub async fn has_idle_eval_only_worker(&self) -> bool {
        self.call(|reply| SchedulerMsg::HasIdleEvalOnlyWorker { reply })
            .await
            .unwrap_or(false)
    }

    pub async fn worker_count(&self) -> usize {
        self.counts().await.workers
    }

    pub async fn workers_info(&self) -> Vec<WorkerInfo> {
        self.board_workers().await
    }

    pub async fn pending_job_count(&self) -> usize {
        self.counts().await.pending
    }
}
