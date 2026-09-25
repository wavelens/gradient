/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A worker's job lifecycle reports, applied in arrival order off the session's
//! read loop. Applying one waits on the graph actor, which under load takes
//! seconds; inline, that wait stopped the connection from reading anything else,
//! so the worker's other jobs timed out on `CacheQuery` replies never read.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_scheduler::Scheduler;
use gradient_types::ids::DispatchedJobId;
use gradient_types::proto::BuildFailureKind;
use gradient_util::shutdown::Shutdown;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::messages::{JobPhaseSpan, JobUpdateKind};

use super::nar_transfer::CommitTracker;
use super::socket::{ProtoWriter, push_pending_candidates};

const JOB_EVENT_QUEUE: usize = 64;
const SLOW_JOB_EVENT: Duration = Duration::from_secs(5);

pub(super) enum JobEvent {
    Update {
        job_id: String,
        update: JobUpdateKind,
    },
    Completed {
        job_id: String,
        dispatch: DispatchedJobId,
        spans: Vec<JobPhaseSpan>,
        commits: Arc<CommitTracker>,
    },
    Failed {
        job_id: String,
        error: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
    },
}

impl JobEvent {
    fn job_id(&self) -> &str {
        match self {
            JobEvent::Update { job_id, .. }
            | JobEvent::Completed { job_id, .. }
            | JobEvent::Failed { job_id, .. } => job_id,
        }
    }
}

pub(super) trait ApplyJobEvent: Send + Sync + 'static {
    fn apply(&self, event: JobEvent) -> impl Future<Output = ()> + Send;
}

pub(super) struct JobEvents {
    tx: Option<mpsc::Sender<JobEvent>>,
    drained: Option<JoinHandle<()>>,
}

impl JobEvents {
    pub(super) fn spawn(shutdown: &Shutdown, peer_id: &str, handler: impl ApplyJobEvent) -> Self {
        let (tx, rx) = mpsc::channel(JOB_EVENT_QUEUE);
        let drained = shutdown.spawn(drain(rx, peer_id.to_owned(), handler));
        Self {
            tx: Some(tx),
            drained: Some(drained),
        }
    }

    pub(super) async fn push(&self, event: JobEvent) {
        let Some(tx) = &self.tx else {
            error!(
                job_id = event.job_id(),
                "job event lane finished; report dropped"
            );
            return;
        };
        if let Err(lost) = tx.send(event).await {
            error!(
                job_id = lost.0.job_id(),
                "job event lane closed; report dropped"
            );
        }
    }

    /// Apply every report already queued. Unregistering the worker re-queues
    /// the jobs it still holds, so a queued failure must land before that.
    pub(super) async fn finish(&mut self) {
        self.tx.take();
        if let Some(drained) = self.drained.take() {
            let _ = drained.await;
        }
    }
}

async fn drain(mut rx: mpsc::Receiver<JobEvent>, peer_id: String, handler: impl ApplyJobEvent) {
    while let Some(event) = rx.recv().await {
        let job_id = event.job_id().to_owned();
        let started = Instant::now();
        handler.apply(event).await;
        let took = started.elapsed();
        if took > SLOW_JOB_EVENT {
            warn!(%peer_id, %job_id, took_ms = took.as_millis() as u64, queued = rx.len(), "job event applied slowly; the graph is behind");
        }
    }
}

pub(super) struct SchedulerJobEvents {
    pub shutdown: Shutdown,
    pub scheduler: Arc<Scheduler>,
    pub writer: ProtoWriter,
    pub peer_id: String,
}

impl ApplyJobEvent for SchedulerJobEvents {
    async fn apply(&self, event: JobEvent) {
        match event {
            JobEvent::Update { job_id, update } => self.update(job_id, update).await,
            JobEvent::Completed {
                job_id,
                dispatch,
                spans,
                commits,
            } => self.completed(job_id, dispatch, spans, commits),
            JobEvent::Failed {
                job_id,
                error,
                kind,
                missing_paths,
            } => self.failed(job_id, error, kind, missing_paths).await,
        }
    }
}

impl SchedulerJobEvents {
    async fn update(&self, job_id: String, update: JobUpdateKind) {
        use gradient_entity::evaluation::EvaluationStatus;

        let (scheduler, peer_id) = (&self.scheduler, self.peer_id.as_str());
        match update {
            JobUpdateKind::Fetching => {
                scheduler
                    .handle_eval_status_update(&job_id, EvaluationStatus::Fetching)
                    .await;
            }
            JobUpdateKind::FetchResult { flake_source } => {
                debug!(%peer_id, %job_id, ?flake_source, "FetchResult");
                scheduler.persist_flake_source(&job_id, flake_source).await;
            }
            JobUpdateKind::EvaluatingFlake => {
                scheduler
                    .handle_eval_status_update(&job_id, EvaluationStatus::EvaluatingFlake)
                    .await;
            }
            JobUpdateKind::EvaluatingDerivations => {
                scheduler
                    .handle_eval_status_update(&job_id, EvaluationStatus::EvaluatingDerivation)
                    .await;
            }
            JobUpdateKind::EvalResult {
                derivations,
                warnings,
                errors,
            } => {
                if let Err(e) = scheduler
                    .handle_eval_result(&job_id, derivations, warnings, errors)
                    .await
                {
                    error!(%peer_id, %job_id, error = %e, "handle_eval_result failed");
                }
                push_pending_candidates(&self.writer, scheduler, peer_id).await;
            }
            JobUpdateKind::Building { build_id } => {
                scheduler
                    .handle_build_status_update(&build_id, peer_id)
                    .await;
            }
            JobUpdateKind::BuildOutput {
                build_id,
                outputs,
                metrics,
                substituted,
            } => {
                if let Err(e) = scheduler
                    .handle_build_output(&job_id, &build_id, outputs, metrics, substituted)
                    .await
                {
                    error!(%peer_id, %job_id, error = %e, "handle_build_output failed");
                }
            }
            JobUpdateKind::Compressing => {}
            JobUpdateKind::EvalStats(report) => {
                if let Err(e) = scheduler.record_eval_metrics(&job_id, report).await {
                    error!(%peer_id, %job_id, error = %e, "record_eval_metrics failed");
                }
            }
            JobUpdateKind::InputUpdateResult {
                candidate_lock,
                bumped,
            } => {
                scheduler
                    .persist_input_update_result(&job_id, candidate_lock, bumped)
                    .await;
            }
            JobUpdateKind::InputUpdateExpansion { matched } => {
                scheduler
                    .persist_input_update_expansion(&job_id, matched)
                    .await;
            }
        }
    }

    fn completed(
        &self,
        job_id: String,
        dispatch: DispatchedJobId,
        spans: Vec<JobPhaseSpan>,
        commits: Arc<CommitTracker>,
    ) {
        let writer = self.writer.clone();
        let scheduler = Arc::clone(&self.scheduler);
        let peer_id = self.peer_id.clone();
        self.shutdown.spawn(async move {
            if !commits.settle(&job_id).await {
                warn!(%peer_id, %job_id, "a NAR this job pushed never reached the index; the build was failed, not completed");
                return;
            }

            info!(%peer_id, %job_id, phases = spans.len(), "job completed");
            scheduler
                .close_job_timeline(dispatch, DispatchedJobOutcome::Completed, spans)
                .await;
            if let Err(e) = scheduler.handle_job_completed(&peer_id, &job_id).await {
                error!(%peer_id, %job_id, error = %e, "handle_job_completed failed");
            }
            push_pending_candidates(&writer, &scheduler, &peer_id).await;
        });
    }

    async fn failed(
        &self,
        job_id: String,
        error: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
    ) {
        let peer_id = self.peer_id.as_str();
        if let Err(e) = self
            .scheduler
            .handle_job_failed(peer_id, &job_id, &error, kind, &missing_paths)
            .await
        {
            error!(%peer_id, %job_id, error = %e, "handle_job_failed failed");
        }
        push_pending_candidates(&self.writer, &self.scheduler, peer_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{Notify, mpsc::UnboundedSender};

    struct Recording {
        applied: UnboundedSender<String>,
        gate: Arc<Notify>,
    }

    impl ApplyJobEvent for Recording {
        async fn apply(&self, event: JobEvent) {
            if event.job_id() == "blocked" {
                self.gate.notified().await;
            }
            let _ = self.applied.send(event.job_id().to_owned());
        }
    }

    fn update(job_id: &str) -> JobEvent {
        JobEvent::Update {
            job_id: job_id.to_owned(),
            update: JobUpdateKind::Compressing,
        }
    }

    fn lane() -> (JobEvents, mpsc::UnboundedReceiver<String>, Arc<Notify>) {
        let (applied, rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Notify::new());
        let events = JobEvents::spawn(
            &Shutdown::new(),
            "w1",
            Recording {
                applied,
                gate: Arc::clone(&gate),
            },
        );
        (events, rx, gate)
    }

    #[tokio::test]
    async fn a_report_is_accepted_while_an_earlier_one_is_still_applying() {
        let (events, _applied, _gate) = lane();
        events.push(update("blocked")).await;

        tokio::time::timeout(Duration::from_secs(1), events.push(update("next")))
            .await
            .expect("the read loop must not wait for the graph");
    }

    #[tokio::test]
    async fn reports_apply_in_the_order_they_arrived() {
        let (events, mut applied, gate) = lane();
        for job_id in ["blocked", "a", "b"] {
            events.push(update(job_id)).await;
        }
        gate.notify_one();

        let mut order = Vec::new();
        for _ in 0..3 {
            order.push(applied.recv().await.unwrap());
        }
        assert_eq!(order, ["blocked", "a", "b"]);
    }

    #[tokio::test]
    async fn finishing_applies_every_queued_report_first() {
        let (mut events, mut applied, gate) = lane();
        for job_id in ["blocked", "failed"] {
            events.push(update(job_id)).await;
        }
        gate.notify_one();

        events.finish().await;

        assert_eq!(applied.try_recv().unwrap(), "blocked");
        assert_eq!(applied.try_recv().unwrap(), "failed");
    }
}
