/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::executor::input_fetch::{
    InputDownloader, InputFetchJob, SubprocessDownloader, UpdaterFetchReport, fetch_input,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gradient_wire::messages::{
    BuildFailureKind, CachedPath, ClientMessage, ClusterAddress, ClusterMembership, ClusterPeer,
    Job, JobCandidate, JobKind, JobPhaseSpan, ServerMessage,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::WorkerConfig;
use crate::executor::eval_split::SplitMember;
use crate::executor::timeline::{JobTimeline, TimelineSnapshot};
use crate::executor::{AbortSignal, JobExecutor};
use crate::exit_clock;
use crate::proto::credentials::CredentialStore;
use crate::proto::job::JobUpdater;
use crate::proto::scorer::JobScorer;
use gradient_worker_client::connection::{ProtoReader, ProtoWriter};
use gradient_worker_client::correlation::AssignmentHandle;

use super::cluster::{ClusterChannels, ClusterHolds, HeldJob};
use super::jobs::{Readoption, WorkerJobs, report_when_drained};
use super::scoring::spawn_scoring_task;

const STOP_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LoopEnd {
    pub(super) draining: bool,
    pub(super) refused: bool,
}

pub(super) async fn run_message_loop(
    mut state: MessageLoopState,
    mut reader: ProtoReader,
    job_session: u64,
    stop: CancellationToken,
) -> Result<LoopEnd> {
    let jobs = Arc::clone(&state.jobs);
    let mut done_rx = jobs.done_rx.lock().await;

    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;

    let mut stop_deadline = None;

    let limits = state.config.exit_limits();
    let mut exit_check = tokio::time::interval(Duration::from_secs(1));

    info!("entering dispatch loop");

    loop {
        tokio::select! {
            biased;

            _ = stop.cancelled(), if !state.stopping => {
                state.begin_stop().await?;
                if state.jobs.registry.lock().is_idle() {
                    break;
                }
                stop_deadline = Some(tokio::time::Instant::now() + STOP_BUDGET);
            }

            () = stop_budget_elapsed(stop_deadline) => {
                warn!(active = state.jobs.registry.lock().len(), "stop budget expired; closing without the remaining job reports");
                break;
            }

            () = jobs.writer.lost(job_session) => {
                warn!("a job report could not be sent; ending the session");
                break;
            }

            Some((job_id, result)) = done_rx.recv() => {
                state.on_job_done(job_id, result).await?;
                if state.stopping && state.jobs.registry.lock().is_idle() {
                    info!("stopped: every aborted job has reported");
                    break;
                }
            }

            _ = exit_check.tick(), if limits.is_set() => {
                state.report_busy();
                if exit_clock::clock().exit_due(&limits, Instant::now()) {
                    if !state.exiting {
                        info!(?limits, "exit limit reached; draining before exit");
                        state.exiting = true;
                        state.begin_drain().await?;
                        state.report_busy();
                    }
                    if exit_clock::clock().is_idle() {
                        info!("exit limit reached and every job has finished; exiting");
                        stop.cancel();
                        break;
                    }
                }
            }

            _ = heartbeat.tick() => {
                state.on_heartbeat().await?;
            }

            inbound = reader.recv() => {
                let Some(inbound) = inbound else {
                    info!("server closed connection");
                    break;
                };
                let started = std::time::Instant::now();
                let kind = inbound.variant_name();
                let result = match state.jobs.nar_recv.absorb(inbound).await {
                    None => Ok(()),
                    Some(msg) => state.route(msg).await,
                };
                let elapsed_ms = started.elapsed().as_millis();
                if elapsed_ms > 1_000 {
                    warn!(kind, elapsed_ms, "slow message dispatch - loop was blocked");
                }
                result?;
            }
        }
        state.report_busy();
    }

    state.jobs.report_busy(0);
    Ok(LoopEnd {
        draining: state.draining && !state.stopping,
        refused: state.refused,
    })
}

async fn stop_budget_elapsed(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(super) struct MessageLoopState {
    writer: ProtoWriter,
    jobs: Arc<WorkerJobs>,
    holds: ClusterHolds,
    channels: ClusterChannels,
    max_eval: u32,
    max_build: u32,
    credentials: CredentialStore,
    scorer: JobScorer,
    executor: JobExecutor,
    downloader: Arc<dyn InputDownloader>,
    fetches: tokio::task::JoinSet<()>,
    running_fetches: Arc<std::sync::atomic::AtomicU32>,
    config: WorkerConfig,
    draining: bool,
    stopping: bool,
    refused: bool,
    exiting: bool,
}

impl MessageLoopState {
    pub(super) fn new(
        writer: ProtoWriter,
        jobs: &Arc<WorkerJobs>,
        config: WorkerConfig,
        executor: JobExecutor,
        scorer: JobScorer,
        credentials: CredentialStore,
    ) -> Self {
        let channels = ClusterChannels::new(writer.clone());
        Self {
            writer,
            jobs: Arc::clone(jobs),
            holds: ClusterHolds::default(),
            channels,
            max_eval: config.eval.max_concurrent.get(),
            max_build: config.build.max_concurrent.get(),
            credentials,
            scorer,
            downloader: Arc::new(SubprocessDownloader(
                executor.evaluator.resolver().config().clone(),
            )),
            fetches: tokio::task::JoinSet::new(),
            running_fetches: Arc::default(),
            executor,
            config,
            draining: false,
            stopping: false,
            refused: false,
            exiting: false,
        }
    }

    fn report_busy(&self) {
        let held = self.holds.held(&JobKind::Flake) + self.holds.held(&JobKind::Build);
        self.jobs
            .report_busy(usize::try_from(held).unwrap_or(usize::MAX));
    }

    async fn begin_drain(&mut self) -> Result<()> {
        self.draining = true;
        let held = self.holds.release_all();
        self.report_released(held, "worker draining").await?;
        self.writer.send(ClientMessage::Draining).await
    }

    async fn begin_stop(&mut self) -> Result<()> {
        self.stopping = true;
        self.begin_drain().await?;
        self.fetches.abort_all();
        let aborted = self.jobs.registry.lock().abort_all();
        info!(
            aborted,
            "stopping: aborted every running job, collecting their reports"
        );
        Ok(())
    }

    fn occupied(&self, kind: JobKind) -> u32 {
        let fetches = match kind {
            JobKind::Build => self
                .running_fetches
                .load(std::sync::atomic::Ordering::Acquire),
            JobKind::Flake => 0,
        };
        self.jobs.registry.lock().active(kind.clone()) + self.holds.held(&kind) + fetches
    }

    fn max_for(&self, kind: JobKind) -> u32 {
        match kind {
            JobKind::Flake => self.max_eval,
            JobKind::Build => self.max_build,
        }
    }

    async fn route(&mut self, msg: ServerMessage) -> Result<()> {
        match msg {
            ServerMessage::JobListChunk {
                candidates,
                is_final,
            } => {
                self.on_job_list_chunk(candidates, is_final);
            }
            ServerMessage::JobOffer { candidates } => {
                self.on_job_offer(candidates);
            }
            ServerMessage::AssignJob {
                job_id,
                assignment_id,
                job,
                cluster,
            } => {
                self.on_assign_job(job_id, assignment_id, job, cluster.map(|m| *m))
                    .await?;
            }
            ServerMessage::StartCluster { attempt, roster } => {
                self.on_start_cluster(attempt, roster).await?;
            }
            ServerMessage::ClusterSignal {
                attempt,
                from,
                payload,
            } => {
                if !self.channels.deliver(&attempt, from, payload.into()) {
                    debug!(%attempt, "ClusterSignal for an attempt with no running member - dropped");
                }
            }
            ServerMessage::AbortCluster { attempt, reason } => {
                self.on_abort_cluster(attempt, reason).await?;
            }
            ServerMessage::AbortJob { job_id, reason } => {
                self.on_abort_job(job_id, reason).await?;
            }
            ServerMessage::Credential { kind, data } => {
                self.on_credential(kind, data.into());
            }
            ServerMessage::Draining => {
                info!("server is draining; finishing in-flight work then disconnecting");
                self.draining = true;
            }
            ServerMessage::Error { code, message } => {
                error!(code, %message, "protocol error from server");
            }
            ServerMessage::Reject { code, reason } => {
                warn!(code, %reason, "server refused the session");
                self.refused = true;
            }
            ServerMessage::InitAck { .. } | ServerMessage::Authenticate { .. } => {
                warn!("unexpected handshake message in dispatch loop - ignoring");
            }
            ServerMessage::AuthChallenge { peers } => {
                self.on_auth_challenge(peers).await?;
            }
            ServerMessage::AuthUpdate {
                authorized_peers,
                failed_peers,
            } => {
                self.on_auth_update(authorized_peers, failed_peers);
            }
            ServerMessage::CacheStatus { query_id, cached } => {
                self.on_cache_status(query_id, cached);
            }
            ServerMessage::CacheError { query_id, message } => {
                self.on_cache_error(query_id, message);
            }
            msg @ (ServerMessage::UploadGrant { .. } | ServerMessage::UploadCommitted { .. }) => {
                self.jobs.uploads.deliver(msg);
            }
            ServerMessage::KnownDerivations { query_id, known } => {
                self.on_known_derivations(query_id, known);
            }
            ServerMessage::EvalCachePullResult { job_id, outcome } => {
                self.jobs
                    .eval_cache_recv
                    .deliver_pull_result(&job_id, outcome);
            }
            ServerMessage::EvalCacheChunk {
                job_id,
                data,
                offset,
                is_final,
            } => {
                self.jobs
                    .eval_cache_recv
                    .deliver_pull_chunk(&job_id, &data, offset, is_final);
            }
            ServerMessage::EvalShardReply { job_id, outcome } => {
                if !self.jobs.eval_shard_replies.deliver(&job_id, outcome) {
                    debug!(%job_id, "EvalShardReply arrived after its waiter cleared");
                }
            }
            ServerMessage::NarPush { .. }
            | ServerMessage::NarStreamHeader { .. }
            | ServerMessage::NarUnavailable { .. }
            | ServerMessage::NarAbort { .. } => {
                warn!("a NAR frame reached the control dispatch");
            }
            ServerMessage::ImportResult {
                job_id,
                request_id,
                outcome,
            } => {
                self.on_import_result(job_id, request_id, outcome);
            }
            ServerMessage::InputResult {
                job_id,
                request_id,
                outcome,
            } => {
                if !gradient_worker_client::correlation::deliver_input_result(
                    &self.jobs.input_waiters,
                    &request_id,
                    outcome,
                ) {
                    debug!(%job_id, %request_id, "InputResult arrived after its waiter cleared");
                }
            }
            ServerMessage::InputProgress {
                request_id,
                bytes_done,
                bytes_total,
                ..
            } => {
                gradient_worker_client::correlation::deliver_input_progress(
                    &self.jobs.input_waiters,
                    &request_id,
                    bytes_done,
                    bytes_total,
                );
            }
            ServerMessage::FetchInput {
                fetch_id,
                project,
                request,
                ssh_key,
            } => {
                self.on_fetch_input(InputFetchJob {
                    fetch_id,
                    project,
                    request,
                    ssh_key,
                });
            }
        }
        Ok(())
    }

    async fn on_job_done(&mut self, job_id: String, result: Result<()>) -> Result<()> {
        let job = self
            .jobs
            .registry
            .lock()
            .finish(&job_id)
            .expect("a job task is registered before it is spawned");
        if let Some(attempt) = &job.cluster {
            self.channels.close(attempt);
        }
        self.jobs.forget_job(&job_id);
        job.log.remove();
        self.credentials.clear();

        let completed_kind = job.kind;
        let assignment_id = job.assignment_id.get();
        let dropped_spans = job.timeline.dropped();
        let TimelineSnapshot { spans, elapsed_ms } = job.timeline.snapshot();

        if dropped_spans > 0 {
            debug!(%job_id, dropped_spans, "phase timeline hit its span cap");
        }

        match result {
            Ok(()) => {
                info!(%job_id, phases = spans.len(), "job completed");
                let completed = ClientMessage::JobCompleted {
                    job_id,
                    assignment_id,
                    spans,
                    elapsed_ms,
                };
                self.jobs
                    .send_final_report(completed_kind.clone(), completed)
                    .await;
            }
            Err(e) => {
                let failed = self.failure_report(job_id, assignment_id, &e, spans, elapsed_ms);
                self.jobs
                    .send_final_report(completed_kind.clone(), failed)
                    .await;
            }
        }

        if !self.draining {
            let kind = completed_kind;
            if self.occupied(kind.clone()) < self.max_for(kind.clone()) {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
        }

        Ok(())
    }

    fn failure_report(
        &self,
        job_id: String,
        assignment_id: String,
        e: &anyhow::Error,
        spans: Vec<JobPhaseSpan>,
        elapsed_ms: u64,
    ) -> ClientMessage {
        let (kind, missing_paths) = crate::executor::failure::wire_failure(e);
        let kind = if self.stopping {
            BuildFailureKind::Canceled
        } else {
            kind
        };

        let error = match kind {
            BuildFailureKind::Canceled => {
                info!(%job_id, phases = spans.len(), "job stopped; reported as canceled");
                "the worker stopped before the job finished".to_owned()
            }
            _ => {
                let error = format!("{e:#}");
                error!(%job_id, %error, ?kind, phases = spans.len(), "job failed");
                error
            }
        };
        ClientMessage::JobFailed {
            job_id,
            assignment_id,
            error,
            kind,
            missing_paths,
            spans,
            elapsed_ms,
            metrics: crate::executor::failure::failure_metrics(e),
        }
    }

    /// A failed send is ending the session.
    /// The control lane is only staying full past its timeout when the peer has stopped reading.
    /// A peer that stalled without closing is never ending the read half.
    /// Warning and carrying on left the worker building jobs it could no longer report.
    async fn on_heartbeat(&mut self) -> Result<()> {
        send_live_metrics(
            &self.writer,
            self.config.build.cgroup.clone(),
            self.config.exit_limits(),
        );
        let expired = self.holds.expired(std::time::Instant::now());
        self.report_released(expired, "cluster start timed out")
            .await?;
        if self.draining {
            return Ok(());
        }
        let active_eval = self.occupied(JobKind::Flake);
        let active_build = self.occupied(JobKind::Build);
        let want_eval = active_eval < self.max_eval;
        let want_build = active_build < self.max_build;
        debug!(
            active_eval,
            max_eval = self.max_eval,
            active_build,
            max_build = self.max_build,
            request_eval = want_eval,
            request_build = want_build,
            "heartbeat tick"
        );
        if want_eval {
            self.writer
                .send(ClientMessage::RequestJob {
                    kind: JobKind::Flake,
                })
                .await
                .context("heartbeat RequestJob Flake")?;
        }
        if want_build {
            self.writer
                .send(ClientMessage::RequestJob {
                    kind: JobKind::Build,
                })
                .await
                .context("heartbeat RequestJob Build")?;
        }

        Ok(())
    }

    fn on_job_list_chunk(&mut self, cands: Vec<JobCandidate>, is_final: bool) {
        debug!(count = cands.len(), is_final, "received job list chunk");
        if self.draining {
            return;
        }
        spawn_scoring_task(
            self.scorer,
            Arc::clone(&self.executor.store),
            self.writer.clone(),
            cands,
            is_final,
            Vec::new(),
        );
    }

    #[tracing::instrument(level = "debug", skip_all, fields(candidates = cands.len()))]
    fn on_job_offer(&mut self, cands: Vec<JobCandidate>) {
        debug!(count = cands.len(), "received job offer");
        if self.draining || cands.is_empty() {
            return;
        }
        let request_after: Vec<JobKind> = [
            (JobKind::Build, self.max_build),
            (JobKind::Flake, self.max_eval),
        ]
        .into_iter()
        .filter(|(kind, max)| self.occupied(kind.clone()) < *max)
        .map(|(kind, _)| kind)
        .collect();
        spawn_scoring_task(
            self.scorer,
            Arc::clone(&self.executor.store),
            self.writer.clone(),
            cands,
            true,
            request_after,
        );
    }

    async fn on_assign_job(
        &mut self,
        job_id: String,
        assignment_id: String,
        job: Job,
        cluster: Option<ClusterMembership>,
    ) -> Result<()> {
        let readoption = self.jobs.registry.lock().readopt(&job_id, &assignment_id);
        match readoption {
            Readoption::Readopted => {
                warn!(%job_id, dispatch = %assignment_id, "job assigned again while still running; reporting under the new dispatch id");
                self.writer
                    .send(ClientMessage::AssignJobResponse {
                        job_id,
                        accepted: true,
                        reason: None,
                    })
                    .await?;
                return Ok(());
            }
            Readoption::Aborting => {
                warn!(%job_id, dispatch = %assignment_id, "rejecting assigned job - its aborted run has not stopped yet");
                self.writer
                    .send(ClientMessage::AssignJobResponse {
                        job_id,
                        accepted: false,
                        reason: Some("an aborted run of this job is still stopping".to_owned()),
                    })
                    .await?;
                return Ok(());
            }
            Readoption::NotRunning => {}
        }

        if self.draining {
            warn!(%job_id, "rejecting assigned job - draining");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted: false,
                    reason: Some("worker is draining".to_owned()),
                })
                .await?;
            return Ok(());
        }

        let kind = match &job {
            Job::Flake(_) => JobKind::Flake,
            Job::Build(_) => JobKind::Build,
        };
        let active_count = self.occupied(kind.clone());
        let max = self.max_for(kind.clone());

        if active_count >= max {
            warn!(%job_id, ?kind, active = active_count, limit = max, "rejecting assigned job - at capacity");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted: false,
                    reason: Some(format!("at capacity ({}/{})", active_count, max)),
                })
                .await?;
            return Ok(());
        }

        if let Some(membership) = cluster {
            let attempt = membership.attempt.clone();
            let held = HeldJob {
                job_id: job_id.clone(),
                assignment_id,
                job,
                kind: kind.clone(),
                credentials: self.credentials.snapshot(),
            };
            let accepted = self.holds.hold(membership, held, std::time::Instant::now());
            info!(%job_id, %attempt, accepted, "cluster member assigned - holding its slot");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted,
                    reason: (!accepted)
                        .then(|| "already holds a member of this cluster attempt".to_owned()),
                })
                .await?;
            if accepted && active_count + 1 < max {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
            return Ok(());
        }

        info!(%job_id, ?kind, "job assigned - accepting");
        self.writer
            .send(ClientMessage::AssignJobResponse {
                job_id: job_id.clone(),
                accepted: true,
                reason: None,
            })
            .await?;

        let credentials = self.credentials.clone();
        self.spawn_job(
            job_id.clone(),
            kind.clone(),
            assignment_id,
            job,
            None,
            None,
            credentials,
        );

        if active_count + 1 < max {
            self.writer.send(ClientMessage::RequestJob { kind }).await?;
        }

        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    fn spawn_job(
        &mut self,
        job_id: String,
        kind: JobKind,
        assignment_id: String,
        job: Job,
        cluster: Option<String>,
        split: Option<SplitMember>,
        credentials: CredentialStore,
    ) {
        let registered = self
            .jobs
            .register(job_id.clone(), kind, assignment_id, cluster);
        let executor = self.executor.clone();
        let mut updater = self.jobs.updater(
            job_id.clone(),
            registered.assignment_id,
            registered.timeline,
            registered.log.clone(),
            Some(Arc::clone(&executor.store)),
        );
        let (log, abort, done_tx) = (registered.log, registered.abort, registered.done_tx);

        #[expect(
            clippy::disallowed_methods,
            reason = "reports back on done_tx and aborts through the registry"
        )]
        tokio::spawn(async move {
            let result = run_job(executor, job, &mut updater, &credentials, abort, split).await;
            report_when_drained(log, done_tx, (job_id, result)).await;
        });
    }

    fn on_fetch_input(&mut self, fetch: InputFetchJob) {
        while self.fetches.try_join_next().is_some() {}
        let updater = self.jobs.updater(
            fetch.fetch_id.clone(),
            AssignmentHandle::new(String::new()),
            JobTimeline::new(),
            self.jobs.log(&fetch.fetch_id),
            Some(Arc::clone(&self.executor.store)),
        );
        let downloader = Arc::clone(&self.downloader);
        let writer = self.writer.clone();
        let binpath_ssh = self.executor.binpath_ssh.clone();
        let running = RunningFetch::start(Arc::clone(&self.running_fetches));
        self.fetches.spawn(async move {
            let _running = running;
            let fetch_id = fetch.fetch_id.clone();
            let report = UpdaterFetchReport {
                updater,
                session: writer,
            };
            let outcome = fetch_input(fetch, &*downloader, &report, &binpath_ssh).await;
            report.updater.uploads.forget_job(&fetch_id);
            if let Err(e) = report
                .session
                .send(ClientMessage::InputFetched { fetch_id, outcome })
                .await
            {
                warn!(error = %e, "input fetch answer not sent");
            }
        });
    }

    async fn on_start_cluster(&mut self, attempt: String, roster: Vec<ClusterPeer>) -> Result<()> {
        let Some((membership, held)) = self.holds.start(&attempt) else {
            warn!(%attempt, "StartCluster for an attempt this worker does not hold - ignoring");
            return Ok(());
        };
        info!(%attempt, job_id = %held.job_id, peers = roster.len(), ?roster, "cluster started - running the held member");
        let split = SplitMember::of_role(&membership.role);
        let me = ClusterAddress {
            role: membership.role,
            index: membership.index,
        };
        self.channels.open(&attempt, me, roster);
        self.spawn_job(
            held.job_id,
            held.kind,
            held.assignment_id,
            held.job,
            Some(attempt),
            split,
            held.credentials,
        );

        Ok(())
    }

    async fn on_abort_cluster(&mut self, attempt: String, reason: String) -> Result<()> {
        let held = self.holds.drop_attempt(&attempt);
        let running = self.jobs.registry.lock().abort_attempt(&attempt);
        self.channels.close(&attempt);
        warn!(%attempt, %reason, held = held.is_some(), running, "cluster attempt aborted by server");
        if let Some(job) = held
            && !self.draining
        {
            self.writer
                .send(ClientMessage::RequestJob { kind: job.kind })
                .await?;
        }

        Ok(())
    }

    async fn report_released(&mut self, released: Vec<HeldJob>, error: &str) -> Result<()> {
        for job in released {
            warn!(job_id = %job.job_id, %error, "releasing a held cluster member");
            self.jobs
                .writer
                .send(ClientMessage::JobFailed {
                    job_id: job.job_id,
                    assignment_id: job.assignment_id,
                    error: error.to_owned(),
                    kind: BuildFailureKind::Aborted,
                    missing_paths: Vec::new(),
                    spans: Vec::new(),
                    elapsed_ms: 0,
                    metrics: None,
                })
                .await?;
        }

        Ok(())
    }

    async fn on_abort_job(&mut self, job_id: String, reason: String) -> Result<()> {
        warn!(%job_id, %reason, "job aborted by server");
        if let Some(held) = self.holds.drop_job(&job_id) {
            let kind = held.kind.clone();
            self.report_released(vec![held], "aborted by server")
                .await?;
            if !self.draining {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
            return Ok(());
        }
        if !self.jobs.abort_job(&job_id) {
            debug!(%job_id, "abort for a job this worker does not run");
        }
        self.jobs.uploads.cancel_job(&job_id).await;

        Ok(())
    }

    fn on_credential(&mut self, kind: gradient_wire::messages::CredentialKind, data: Vec<u8>) {
        debug!(?kind, "received credential");
        self.credentials.store(kind, data);
    }

    fn on_cache_status(&mut self, query_id: String, cached: Vec<CachedPath>) {
        let count = cached.len();
        if !gradient_worker_client::correlation::deliver_cache_reply(
            &self.jobs.cache_waiters,
            &query_id,
            Ok(cached),
        ) {
            debug!(%query_id, count, "CacheStatus arrived after waiter cleared");
        }
    }

    fn on_cache_error(&mut self, query_id: String, message: String) {
        if !gradient_worker_client::correlation::deliver_cache_reply(
            &self.jobs.cache_waiters,
            &query_id,
            Err(message),
        ) {
            debug!(%query_id, "CacheError arrived after waiter cleared");
        }
    }

    fn on_known_derivations(&mut self, query_id: String, known: Vec<String>) {
        let count = known.len();
        if !gradient_worker_client::correlation::deliver_known_derivations(
            &self.jobs.known_derivation_waiters,
            &query_id,
            known,
        ) {
            debug!(%query_id, count, "KnownDerivations arrived after waiter cleared");
        }
    }

    fn on_import_result(
        &mut self,
        job_id: String,
        request_id: String,
        outcome: gradient_wire::types::ImportOutcome,
    ) {
        if !gradient_worker_client::correlation::deliver_import_result(
            &self.jobs.import_waiters,
            &request_id,
            outcome,
        ) {
            debug!(%job_id, %request_id, "ImportResult arrived after its waiter cleared");
        }
    }

    async fn on_auth_challenge(&mut self, peers: Vec<String>) -> Result<()> {
        debug!(
            ?peers,
            "mid-connection AuthChallenge - sending AuthResponse"
        );
        let peer_tokens = self.config.peer_tokens();
        let tokens = gradient_worker_client::connection::handshake::resolve_tokens_for_challenge(
            &peer_tokens,
            &peers,
        );
        self.writer
            .send(ClientMessage::AuthResponse { tokens })
            .await?;
        Ok(())
    }

    fn on_auth_update(
        &mut self,
        authorized_peers: Vec<String>,
        failed_peers: Vec<gradient_wire::messages::FailedPeer>,
    ) {
        info!(
            authorized = authorized_peers.len(),
            failed = failed_peers.len(),
            "auth updated"
        );
        for fp in &failed_peers {
            warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
        }
    }
}

struct RunningFetch(Arc<std::sync::atomic::AtomicU32>);

impl RunningFetch {
    fn start(count: Arc<std::sync::atomic::AtomicU32>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(count)
    }
}

impl Drop for RunningFetch {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn send_live_metrics(
    writer: &ProtoWriter,
    daemon_cgroup: Option<std::path::PathBuf>,
    limits: exit_clock::ExitLimits,
) {
    let writer = writer.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "one sample per heartbeat, never awaited"
    )]
    tokio::spawn(async move {
        let sampled = tokio::task::spawn_blocking(move || {
            (
                crate::metrics::host_dynamic(),
                crate::executor::peak_ram_of_running_builds(daemon_cgroup.as_deref()),
            )
        });
        let (m, build_peak_ram_mb) = match sampled.await {
            Ok(sampled) => sampled,
            Err(e) => {
                debug!(error = %e, "host_dynamic sampling task failed");
                return;
            }
        };
        if let Err(e) = writer
            .send(ClientMessage::WorkerMetrics {
                cpu_usage_pct: m.cpu_usage_pct,
                ram_free_mb: m.ram_free_mb,
                disk_speed_mbps: gradient_worker_client::throughput::DISK.current(),
                upload_speed_mbps: gradient_worker_client::throughput::UPLOAD.current(),
                download_speed_mbps: gradient_worker_client::throughput::DOWNLOAD.current(),
                build_peak_ram_mb,
                lifetime_left_secs: exit_clock::clock()
                    .lifetime_left(&limits, Instant::now())
                    .map(|left| left.as_secs()),
            })
            .await
        {
            debug!(error = %e, "heartbeat WorkerMetrics send failed");
        }
    });
}

async fn run_job(
    executor: JobExecutor,
    job: Job,
    updater: &mut JobUpdater,
    credentials: &CredentialStore,
    abort: AbortSignal,
    split: Option<SplitMember>,
) -> Result<()> {
    let job_abort = abort.clone();
    let run = async move {
        match job {
            Job::Flake(flake_job) => {
                executor
                    .execute_flake_job(flake_job, updater, credentials, job_abort, split)
                    .await
            }
            Job::Build(build_job) => {
                executor
                    .execute_build_job(build_job, updater, credentials, job_abort)
                    .await
            }
        }
    };
    until_aborted(run, abort).await
}

pub(super) async fn until_aborted(
    job: impl std::future::Future<Output = Result<()>>,
    mut abort: AbortSignal,
) -> Result<()> {
    abort.check()?;

    tokio::select! {
        biased;
        result = job => result,
        () = abort.aborted() => abort.check(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::failure::JobAborted;

    #[tokio::test]
    async fn an_aborted_job_stops_without_reaching_a_checkpoint() {
        let (abort, abort_rx) = AbortSignal::channel();
        abort.send(true).expect("the job listens");

        let err = until_aborted(std::future::pending(), abort_rx)
            .await
            .expect_err("an aborted job fails");

        assert!(
            err.downcast_ref::<JobAborted>().is_some(),
            "reported as an abort, not a failure: {err:#}"
        );
    }

    #[tokio::test]
    async fn a_job_nobody_aborts_reports_its_own_result() {
        let (_abort, abort_rx) = AbortSignal::channel();

        let result = until_aborted(async { Err(anyhow::anyhow!("nix failed")) }, abort_rx).await;

        assert_eq!(
            format!("{:#}", result.expect_err("its own error")),
            "nix failed"
        );
    }

    struct StalledDownloader;

    #[async_trait::async_trait]
    impl InputDownloader for StalledDownloader {
        async fn download(
            &self,
            _request: String,
            _git_ssh_command: Option<String>,
            _sink: Arc<dyn gradient_eval::pool::DownloadSink>,
        ) -> Result<String> {
            std::future::pending().await
        }
    }

    struct OfflineDownloader;

    #[async_trait::async_trait]
    impl InputDownloader for OfflineDownloader {
        async fn download(
            &self,
            _request: String,
            _git_ssh_command: Option<String>,
            _sink: Arc<dyn gradient_eval::pool::DownloadSink>,
        ) -> Result<String> {
            Err(anyhow::anyhow!("offline"))
        }
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test stands in for the server by hand"
    )]
    async fn an_input_fetch_runs_while_the_eval_pool_is_full() {
        let server = gradient_test_support::prelude::MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let answered = tokio::spawn(async move {
            let mut sc = server.accept().await;
            sc.recv().await
        });
        let conn = gradient_worker_client::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, _reader, _flush) = conn.split();
        let gcroots = tempfile::tempdir().unwrap();
        let mut config = WorkerConfig {
            base_dir: gcroots.path().to_owned(),
            ..WorkerConfig::default()
        };
        config.eval.max_concurrent = std::num::NonZeroU32::MIN;
        let jobs = Arc::new(WorkerJobs::new(&config));
        let mut state = MessageLoopState::new(
            writer,
            &jobs,
            config,
            super::super::test_executor(gcroots.path()),
            JobScorer::new(),
            CredentialStore::new(),
        );
        state.downloader = Arc::new(OfflineDownloader);
        let _running = state.jobs.register(
            "eval:e1".to_owned(),
            JobKind::Flake,
            "dispatch-1".to_owned(),
            None,
        );
        assert_eq!(
            state.occupied(JobKind::Flake),
            state.max_for(JobKind::Flake)
        );

        state
            .route(ServerMessage::FetchInput {
                fetch_id: "f1".into(),
                project: "p1".into(),
                request: "{}".into(),
                ssh_key: None,
            })
            .await
            .unwrap();

        let sent = tokio::time::timeout(std::time::Duration::from_secs(5), answered)
            .await
            .expect("the fetch answers while every eval slot is taken")
            .unwrap()
            .unwrap();
        assert_eq!(
            sent,
            ClientMessage::InputFetched {
                fetch_id: "f1".into(),
                outcome: gradient_wire::types::InputOutcome::Failed {
                    message: "offline".into()
                },
            }
        );
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test stands in for the server by hand"
    )]
    async fn a_running_input_fetch_occupies_a_build_slot() {
        let server = gradient_test_support::prelude::MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let _accepted = tokio::spawn(async move {
            let _sc = server.accept().await;
            std::future::pending::<()>().await
        });
        let conn = gradient_worker_client::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, _reader, _flush) = conn.split();
        let gcroots = tempfile::tempdir().unwrap();
        let config = WorkerConfig {
            base_dir: gcroots.path().to_owned(),
            ..WorkerConfig::default()
        };
        let jobs = Arc::new(WorkerJobs::new(&config));
        let mut state = MessageLoopState::new(
            writer,
            &jobs,
            config,
            super::super::test_executor(gcroots.path()),
            JobScorer::new(),
            CredentialStore::new(),
        );
        state.downloader = Arc::new(StalledDownloader);
        let _running = state.jobs.register(
            "build:b1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );

        state
            .route(ServerMessage::FetchInput {
                fetch_id: "f1".into(),
                project: "p1".into(),
                request: "{}".into(),
                ssh_key: None,
            })
            .await
            .unwrap();

        assert_eq!(state.occupied(JobKind::Build), 2);
        assert_eq!(state.occupied(JobKind::Flake), 0);
        state.fetches.abort_all();
        while state.fetches.join_next().await.is_some() {}
        assert_eq!(
            state.occupied(JobKind::Build),
            1,
            "an ended fetch frees its slot"
        );
    }
}
