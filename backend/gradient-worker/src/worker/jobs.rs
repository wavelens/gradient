/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    ClientMessage, GuestChannelKind, JobKind, RUNNING_JOBS_SINCE, RunningAssignment,
};
use gradient_worker_client::connection::ProtoWriter;
use gradient_worker_client::connection::reconnecting::ReconnectingWriter;
use gradient_worker_client::correlation::{
    AssignmentHandle, CacheWaiters, ImportWaiters, InputWaiters, KnownDerivationWaiters,
};
use gradient_worker_client::nar_recv::NarReceiver;
use gradient_worker_client::upload::UploadClient;
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

use crate::config::WorkerConfig;
use crate::count::clamp_to_u32;
use crate::exec::channels::GuestChannelCommand;
use crate::exec::job_channels::GuestChannelInbox;
use crate::executor::AbortSignal;
use crate::executor::timeline::JobTimeline;
use crate::exit_clock;
use crate::nix::store::LocalNixStore;
use crate::proto::eval_cache_recv::EvalCacheReceiver;
use crate::proto::eval_shard_replies::EvalShardReplies;
use crate::proto::job::JobUpdater;
use crate::proto::job_log::{JOB_LOG_BUFFER_CAP, JobLog, file_key};

pub(super) type JobDone = (String, Result<()>);

const GUEST_CHANNEL_INBOX: usize = 256;

pub(super) struct ActiveJob {
    pub(super) kind: JobKind,
    pub(super) assignment_id: AssignmentHandle,
    abort: watch::Sender<bool>,
    pub(super) timeline: Arc<JobTimeline>,
    pub(super) cluster: Option<String>,
    pub(super) log: JobLog,
    guest_channels: Option<mpsc::Sender<GuestChannelCommand>>,
}

pub(super) struct JobRegistry {
    running: HashMap<String, ActiveJob>,
    channel_jobs: HashMap<u64, String>,
    done_tx: mpsc::UnboundedSender<JobDone>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Readoption {
    Readopted,
    Aborting,
    NotRunning,
}

pub(super) struct Registered {
    pub(super) assignment_id: AssignmentHandle,
    pub(super) abort: AbortSignal,
    pub(super) timeline: Arc<JobTimeline>,
    pub(super) log: JobLog,
    pub(super) done_tx: mpsc::UnboundedSender<JobDone>,
    pub(super) guest_channels: GuestChannelInbox,
}

impl JobRegistry {
    pub(super) fn active(&self, kind: JobKind) -> u32 {
        clamp_to_u32(self.running.values().filter(|j| j.kind == kind).count())
    }

    pub(super) fn len(&self) -> usize {
        self.running.len()
    }

    pub(super) fn runs(&self, job_id: &str) -> bool {
        self.running.contains_key(job_id)
    }

    fn register(
        &mut self,
        job_id: String,
        kind: JobKind,
        assignment_id: String,
        cluster: Option<String>,
        log: JobLog,
    ) -> Registered {
        let assignment_id = AssignmentHandle::new(assignment_id);
        let (abort, abort_rx) = AbortSignal::channel();
        let timeline = JobTimeline::new();
        let (commands, inbox) = mpsc::channel(GUEST_CHANNEL_INBOX);
        self.running.insert(
            job_id,
            ActiveJob {
                guest_channels: (kind == JobKind::Exec).then_some(commands),
                kind,
                assignment_id: assignment_id.clone(),
                abort,
                timeline: Arc::clone(&timeline),
                cluster,
                log: log.clone(),
            },
        );
        Registered {
            assignment_id,
            abort: abort_rx,
            timeline,
            log,
            done_tx: self.done_tx.clone(),
            guest_channels: inbox,
        }
    }

    pub(super) fn open_guest_channel(
        &mut self,
        job_id: &str,
        channel_id: u64,
        kind: GuestChannelKind,
    ) -> bool {
        let Some(commands) = self
            .running
            .get(job_id)
            .and_then(|job| job.guest_channels.as_ref())
        else {
            return false;
        };
        if commands
            .try_send(GuestChannelCommand::Open { channel_id, kind })
            .is_err()
        {
            return false;
        }
        self.channel_jobs.insert(channel_id, job_id.to_owned());
        true
    }

    pub(super) fn route_guest_channel(&mut self, command: GuestChannelCommand) -> bool {
        let channel_id = match &command {
            GuestChannelCommand::Data { channel_id, .. }
            | GuestChannelCommand::Resize { channel_id, .. } => *channel_id,
            GuestChannelCommand::Close { channel_id } => {
                let channel_id = *channel_id;
                let routed = self.send_to_channel_job(channel_id, command);
                self.channel_jobs.remove(&channel_id);
                return routed;
            }
            GuestChannelCommand::Open { .. } | GuestChannelCommand::CloseAll => return false,
        };
        self.send_to_channel_job(channel_id, command)
    }

    fn send_to_channel_job(&self, channel_id: u64, command: GuestChannelCommand) -> bool {
        self.channel_jobs
            .get(&channel_id)
            .and_then(|job_id| self.running.get(job_id))
            .and_then(|job| job.guest_channels.as_ref())
            .is_some_and(|commands| commands.try_send(command).is_ok())
    }

    pub(super) fn close_guest_channels(&mut self) {
        self.channel_jobs.clear();
        for commands in self
            .running
            .values()
            .filter_map(|job| job.guest_channels.as_ref())
        {
            if commands.try_send(GuestChannelCommand::CloseAll).is_err() {
                debug!("an exec job did not take the guest channel close");
            }
        }
    }

    pub(super) fn readopt(&self, job_id: &str, assignment_id: &str) -> Readoption {
        match self.running.get(job_id) {
            Some(job) if *job.abort.borrow() => Readoption::Aborting,
            Some(job) => {
                job.assignment_id.set(assignment_id.to_owned());
                Readoption::Readopted
            }
            None => Readoption::NotRunning,
        }
    }

    pub(super) fn abort(&self, job_id: &str) -> bool {
        match self.running.get(job_id) {
            Some(job) => job.abort.send(true).is_ok(),
            None => false,
        }
    }

    pub(super) fn abort_all(&self) -> usize {
        self.aborting(|_| true)
    }

    pub(super) fn abort_for_stop(&self, keep_exec_jobs: bool) -> usize {
        self.aborting(|j| !(keep_exec_jobs && j.kind == JobKind::Exec))
    }

    pub(super) fn is_idle_for_stop(&self, keep_exec_jobs: bool) -> bool {
        self.running
            .values()
            .all(|j| keep_exec_jobs && j.kind == JobKind::Exec)
    }

    pub(super) fn abort_attempt(&self, attempt: &str) -> usize {
        self.aborting(|j| j.cluster.as_deref() == Some(attempt))
    }

    fn abort_cluster_members(&self) -> usize {
        self.aborting(|j| j.cluster.is_some())
    }

    fn aborting(&self, selected: impl Fn(&ActiveJob) -> bool) -> usize {
        self.running
            .values()
            .filter(|j| selected(j))
            .filter(|j| j.abort.send(true).is_ok())
            .count()
    }

    pub(super) fn finish(&mut self, job_id: &str) -> Option<ActiveJob> {
        self.channel_jobs.retain(|_, owner| owner != job_id);
        self.running.remove(job_id)
    }

    fn running_assignments(&self) -> Vec<RunningAssignment> {
        self.running
            .values()
            .filter(|j| j.cluster.is_none() && !*j.abort.borrow())
            .map(|j| RunningAssignment {
                assignment_id: j.assignment_id.get(),
                kind: j.kind.clone(),
            })
            .collect()
    }

    fn logs(&self) -> Vec<JobLog> {
        self.running.values().map(|j| j.log.clone()).collect()
    }
}

pub(crate) struct WorkerJobs {
    pub(super) registry: Mutex<JobRegistry>,
    pub(super) done_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<JobDone>>,
    pub(super) cache_waiters: CacheWaiters,
    pub(super) known_derivation_waiters: KnownDerivationWaiters,
    pub(super) import_waiters: ImportWaiters,
    pub(super) input_waiters: InputWaiters,
    pub(super) nar_recv: NarReceiver,
    pub(super) eval_cache_recv: EvalCacheReceiver,
    pub(super) eval_shard_replies: EvalShardReplies,
    pub(super) uploads: UploadClient,
    pub(super) writer: ReconnectingWriter,
    reporting: Mutex<HashMap<String, JobKind>>,
    log_dir: PathBuf,
    busy_session: u64,
}

pub(super) async fn report_when_drained(
    log: JobLog,
    done_tx: mpsc::UnboundedSender<JobDone>,
    done: JobDone,
) {
    tokio::select! {
        () = log.drained() => {
            let _ = done_tx.send(done);
        }
        () = done_tx.closed() => log.remove(),
    }
}

pub(crate) fn clear_job_logs(config: &WorkerConfig, kept_jobs: &[String]) -> std::io::Result<()> {
    let dir = config.job_log_dir();
    std::fs::create_dir_all(&dir)?;
    let kept: HashSet<String> = kept_jobs.iter().map(|job_id| file_key(job_id)).collect();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let key = name.to_string_lossy();
        if kept.contains(key.split('.').next().unwrap_or_default()) {
            continue;
        }
        match entry.file_type()?.is_dir() {
            true => std::fs::remove_dir_all(entry.path())?,
            false => std::fs::remove_file(entry.path())?,
        }
    }
    Ok(())
}

impl WorkerJobs {
    pub(crate) fn new(config: &WorkerConfig) -> Self {
        let nar_recv = match gradient_storage::PartialStore::new(config.nar_partial_dir()) {
            Ok(store) => NarReceiver::with_partial_store(store),
            Err(e) => {
                warn!(error = %e, "failed to init NAR partial dir; downloads will not resume");
                NarReceiver::new()
            }
        };
        let writer = ReconnectingWriter::new();
        let uploads = UploadClient::new(
            writer.clone(),
            usize::try_from(config.nar.max_concurrent_uploads.get()).unwrap_or(usize::MAX),
        )
        .with_chunk_bytes(usize::try_from(config.nar.chunk_bytes).unwrap_or(usize::MAX));
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            registry: Mutex::new(JobRegistry {
                running: HashMap::new(),
                channel_jobs: HashMap::new(),
                done_tx,
            }),
            done_rx: tokio::sync::Mutex::new(done_rx),
            cache_waiters: CacheWaiters::default(),
            known_derivation_waiters: KnownDerivationWaiters::default(),
            import_waiters: ImportWaiters::default(),
            input_waiters: InputWaiters::default(),
            nar_recv,
            eval_cache_recv: EvalCacheReceiver::new(),
            eval_shard_replies: EvalShardReplies::default(),
            uploads,
            writer,
            reporting: Mutex::default(),
            log_dir: config.job_log_dir(),
            busy_session: exit_clock::new_session(),
        }
    }

    pub(super) fn register(
        &self,
        job_id: String,
        kind: JobKind,
        assignment_id: String,
        cluster: Option<String>,
    ) -> Registered {
        self.writer.forget_reports(&job_id);
        let log = match kind {
            JobKind::Exec => JobLog::open_resumable(&self.log_dir, &job_id, JOB_LOG_BUFFER_CAP),
            JobKind::Flake | JobKind::Build => {
                JobLog::open(&self.log_dir, &job_id, JOB_LOG_BUFFER_CAP)
            }
        };
        self.registry
            .lock()
            .register(job_id, kind, assignment_id, cluster, log)
    }

    pub(super) fn resume_job(
        &self,
        job_id: String,
        assignment_id: String,
        run: impl FnOnce(JobUpdater, AbortSignal, GuestChannelInbox) -> BoxFuture<'static, Result<()>>,
    ) {
        self.writer.forget_reports(&job_id);
        let log = JobLog::reopen(&self.log_dir, &job_id, JOB_LOG_BUFFER_CAP).unwrap_or_else(|e| {
            warn!(%job_id, error = %e, "unreadable log of a resumed exec job, its output starts over");
            JobLog::open_resumable(&self.log_dir, &job_id, JOB_LOG_BUFFER_CAP)
        });
        let registered =
            self.registry
                .lock()
                .register(job_id.clone(), JobKind::Exec, assignment_id, None, log);
        let updater = self.updater(
            job_id.clone(),
            registered.assignment_id,
            registered.timeline,
            registered.log.clone(),
            None,
        );
        let job = run(updater, registered.abort, registered.guest_channels);
        let (log, done_tx) = (registered.log, registered.done_tx);
        #[expect(
            clippy::disallowed_methods,
            reason = "reports back on done_tx and aborts through the registry"
        )]
        tokio::spawn(async move {
            let result = job.await;
            report_when_drained(log, done_tx, (job_id, result)).await;
        });
    }

    pub(super) fn abort_job(&self, job_id: &str) -> bool {
        self.writer.forget_reports(job_id);
        self.registry.lock().abort(job_id)
    }

    pub(super) fn updater(
        &self,
        job_id: String,
        assignment_id: AssignmentHandle,
        timeline: Arc<JobTimeline>,
        log: JobLog,
        store: Option<Arc<LocalNixStore>>,
    ) -> JobUpdater {
        JobUpdater::new(
            job_id,
            assignment_id,
            self.writer.clone(),
            Arc::clone(&self.cache_waiters),
            Arc::clone(&self.known_derivation_waiters),
            Arc::clone(&self.import_waiters),
            Arc::clone(&self.input_waiters),
            self.nar_recv.clone(),
            self.eval_cache_recv.clone(),
            self.eval_shard_replies.clone(),
            store,
            timeline,
            self.uploads.clone(),
            log,
        )
    }

    pub(super) fn log(&self, job_id: &str) -> JobLog {
        JobLog::open(&self.log_dir, job_id, JOB_LOG_BUFFER_CAP)
    }

    pub(super) fn forget_job(&self, job_id: &str) {
        use gradient_worker_client::correlation as c;
        c::forget_cache_waiters_for_job(&self.cache_waiters, job_id);
        c::forget_known_derivation_waiters_for_job(&self.known_derivation_waiters, job_id);
        c::forget_import_waiters_for_job(&self.import_waiters, job_id);
        c::forget_input_waiters_for_job(&self.input_waiters, job_id);
        self.nar_recv.forget_job(job_id);
        self.eval_cache_recv.forget_job(job_id);
        self.eval_shard_replies.forget_job(job_id);
        self.uploads.forget_job(job_id);
    }

    pub(super) fn report_busy(&self, held: usize) {
        let busy = self.registry.lock().len() + held;
        exit_clock::clock().report(self.busy_session, busy, Instant::now());
    }

    pub(super) async fn send_final_report(&self, kind: JobKind, report: ClientMessage) {
        if let ClientMessage::JobCompleted { assignment_id, .. }
        | ClientMessage::JobFailed { assignment_id, .. } = &report
        {
            self.reporting.lock().insert(assignment_id.clone(), kind);
        }
        let _ = self.writer.send(report).await;
    }

    pub(crate) fn running_assignments(&self) -> Vec<RunningAssignment> {
        let mut running = self.registry.lock().running_assignments();
        let queued = self.writer.queued_final_reports();
        let mut reporting = self.reporting.lock();
        reporting.retain(|assignment_id, _| queued.contains(assignment_id));
        running.extend(
            reporting
                .iter()
                .map(|(assignment_id, kind)| RunningAssignment {
                    assignment_id: assignment_id.clone(),
                    kind: kind.clone(),
                }),
        );
        running
    }

    pub(crate) async fn start_session(&self, writer: ProtoWriter) -> Result<u64> {
        self.writer.attach(writer).await?;
        let session = self
            .writer
            .current_session()
            .context("the session ended while it started")?;
        let logs = self.registry.lock().logs();
        for log in logs {
            let writer = self.writer.clone();
            #[expect(
                clippy::disallowed_methods,
                reason = "ends with the replay; a lost session ends it early"
            )]
            tokio::spawn(async move {
                if let Err(e) = log.replay(&writer).await {
                    debug!(error = %e, "job log replay cut short; the next session resumes it");
                }
            });
        }
        Ok(session)
    }

    pub(crate) fn end_session(&self, agreed_version: u16) {
        self.writer.detach();
        let mut registry = self.registry.lock();
        registry.close_guest_channels();
        let members = registry.abort_cluster_members();
        let all = if agreed_version < RUNNING_JOBS_SINCE {
            registry.abort_all()
        } else {
            0
        };
        debug!(
            members,
            all, agreed_version, "session ended; running jobs continue"
        );
    }
}

impl Drop for WorkerJobs {
    fn drop(&mut self) {
        exit_clock::clock().report(self.busy_session, 0, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::failure::JobAborted;

    fn registry() -> (JobRegistry, mpsc::UnboundedReceiver<JobDone>) {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        (
            JobRegistry {
                running: HashMap::new(),
                channel_jobs: HashMap::new(),
                done_tx,
            },
            done_rx,
        )
    }

    fn log() -> JobLog {
        JobLog::open(&std::env::temp_dir(), "unused", JOB_LOG_BUFFER_CAP)
    }

    fn worker_jobs() -> (WorkerJobs, tempfile::TempDir) {
        let base = tempfile::tempdir().unwrap();
        let config = WorkerConfig {
            base_dir: base.path().to_owned(),
            ..WorkerConfig::default()
        };
        (WorkerJobs::new(&config), base)
    }

    #[test]
    fn a_reassigned_running_job_keeps_its_task_and_takes_the_new_assignment_id() {
        let (mut jobs, _done_rx) = registry();
        let registered = jobs.register(
            "job-1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
            log(),
        );

        assert_eq!(jobs.readopt("job-1", "dispatch-2"), Readoption::Readopted);

        assert_eq!(
            registered.assignment_id.get(),
            "dispatch-2",
            "the task reports under the new id"
        );
        assert_eq!(
            jobs.active(JobKind::Build),
            1,
            "no second task is registered"
        );
        assert!(jobs.abort("job-1"));
        assert!(
            registered.abort.is_aborted(),
            "the first task still hears the abort"
        );
        assert_eq!(
            jobs.readopt("job-9", "dispatch-3"),
            Readoption::NotRunning,
            "an unknown job is a fresh assignment"
        );
    }

    #[test]
    fn an_aborted_job_assigned_again_is_refused_and_keeps_its_assignment_id() {
        let (mut jobs, _done_rx) = registry();
        let registered = jobs.register(
            "job-1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
            log(),
        );
        assert!(jobs.abort("job-1"));

        assert_eq!(jobs.readopt("job-1", "dispatch-2"), Readoption::Aborting);

        assert_eq!(
            registered.assignment_id.get(),
            "dispatch-1",
            "the aborted task must not report under the new assignment"
        );
    }

    #[test]
    fn a_kept_exec_job_survives_a_worker_stop() {
        let (mut jobs, _done_rx) = registry();
        let build = jobs.register(
            "build:x".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
            log(),
        );
        let exec = jobs.register(
            "exec:y".to_owned(),
            JobKind::Exec,
            "dispatch-2".to_owned(),
            None,
            log(),
        );

        assert_eq!(jobs.abort_for_stop(true), 1);

        assert!(build.abort.is_aborted());
        assert!(!exec.abort.is_aborted());
        assert!(!jobs.is_idle_for_stop(true));
        jobs.finish("build:x");
        assert!(jobs.is_idle_for_stop(true));
        assert!(!jobs.is_idle_for_stop(false));
    }

    #[test]
    fn a_stopped_worker_aborts_its_exec_jobs_too() {
        let (mut jobs, _done_rx) = registry();
        let build = jobs.register(
            "build:x".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
            log(),
        );
        let exec = jobs.register(
            "exec:y".to_owned(),
            JobKind::Exec,
            "dispatch-2".to_owned(),
            None,
            log(),
        );

        assert_eq!(jobs.abort_for_stop(false), 2);

        assert!(build.abort.is_aborted());
        assert!(exec.abort.is_aborted());
    }

    #[test]
    fn aborting_an_attempt_stops_only_its_members() {
        let (mut jobs, _done_rx) = registry();
        let member = jobs.register(
            "build:x".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            Some("a1".to_owned()),
            log(),
        );
        let single = jobs.register(
            "build:y".to_owned(),
            JobKind::Build,
            "dispatch-2".to_owned(),
            None,
            log(),
        );

        assert_eq!(jobs.abort_attempt("a1"), 1);

        assert!(member.abort.is_aborted(), "the member hears the abort");
        assert!(!single.abort.is_aborted(), "a single job keeps running");
    }

    #[test]
    fn finishing_a_job_hands_back_its_current_assignment_id() {
        let (mut jobs, _done_rx) = registry();
        jobs.register(
            "job-1".to_owned(),
            JobKind::Flake,
            "dispatch-1".to_owned(),
            None,
            log(),
        );
        jobs.readopt("job-1", "dispatch-2");

        let job = jobs.finish("job-1").expect("the job was running");

        assert_eq!(job.assignment_id.get(), "dispatch-2");
        assert_eq!(jobs.len(), 0);
        assert!(jobs.finish("job-1").is_none());
    }

    #[tokio::test]
    async fn dropping_the_jobs_of_a_dialed_worker_stops_every_job() {
        let (jobs, _base) = worker_jobs();
        let waiting = jobs.register(
            "build:x".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );
        let idle = jobs.register(
            "eval:y".to_owned(),
            JobKind::Flake,
            "dispatch-2".to_owned(),
            None,
        );
        let mut job = std::pin::pin!(super::super::message_loop::until_aborted(
            std::future::pending(),
            waiting.abort
        ));
        assert!(futures::poll!(job.as_mut()).is_pending());

        drop(jobs);

        let err = tokio::time::timeout(std::time::Duration::from_secs(5), job)
            .await
            .expect("the job stops once its worker is gone")
            .expect_err("a job of a dropped worker fails");
        assert!(
            err.downcast_ref::<JobAborted>().is_some(),
            "reported as an abort: {err:#}"
        );
        assert!(
            idle.abort.is_aborted(),
            "a job between checkpoints sees it too"
        );
    }

    #[tokio::test]
    async fn a_running_job_survives_the_session_end_and_is_listed() {
        let (jobs, _base) = worker_jobs();
        let registered = jobs.register(
            "job-1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );

        jobs.end_session(RUNNING_JOBS_SINCE);

        assert!(!registered.abort.is_aborted());
        assert_eq!(
            jobs.running_assignments(),
            vec![RunningAssignment {
                assignment_id: "dispatch-1".into(),
                kind: JobKind::Build,
            }]
        );
    }

    #[tokio::test]
    async fn an_older_server_session_end_aborts_every_job() {
        let (jobs, _base) = worker_jobs();
        let registered = jobs.register(
            "job-1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );

        jobs.end_session(RUNNING_JOBS_SINCE - 1);

        assert!(registered.abort.is_aborted());
    }

    #[tokio::test]
    async fn a_cluster_member_is_aborted_when_the_session_ends() {
        let (jobs, _base) = worker_jobs();
        let single = jobs.register(
            "build:x".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );
        let member = jobs.register(
            "build:y".to_owned(),
            JobKind::Build,
            "dispatch-2".to_owned(),
            Some("a1".to_owned()),
        );

        jobs.end_session(RUNNING_JOBS_SINCE);

        assert!(!single.abort.is_aborted());
        assert!(member.abort.is_aborted());
        assert_eq!(jobs.running_assignments().len(), 1);
    }

    #[tokio::test]
    async fn a_final_report_queued_while_detached_keeps_its_assignment_listed() {
        let (jobs, _base) = worker_jobs();
        jobs.send_final_report(
            JobKind::Build,
            ClientMessage::JobCompleted {
                job_id: "build:1".into(),
                assignment_id: "dispatch-1".into(),
                spans: vec![],
                elapsed_ms: 1,
            },
        )
        .await;

        assert_eq!(
            jobs.running_assignments(),
            vec![RunningAssignment {
                assignment_id: "dispatch-1".into(),
                kind: JobKind::Build,
            }],
            "the server must hold the assignment until the report arrives"
        );

        let session = gradient_worker_client::testing::mock_session().await;
        jobs.writer.attach(session.writer.clone()).await.unwrap();
        assert!(jobs.running_assignments().is_empty());
    }

    fn completed(job_id: &str, assignment_id: &str) -> ClientMessage {
        ClientMessage::JobCompleted {
            job_id: job_id.into(),
            assignment_id: assignment_id.into(),
            spans: vec![],
            elapsed_ms: 1,
        }
    }

    async fn reported_into_an_ended_session(jobs: &WorkerJobs) {
        let session = gradient_worker_client::testing::mock_session().await;
        jobs.start_session(session.writer.clone()).await.unwrap();
        jobs.send_final_report(JobKind::Build, completed("build:1", "dispatch-1"))
            .await;
        jobs.end_session(RUNNING_JOBS_SINCE);
    }

    #[tokio::test]
    async fn a_final_report_sent_before_the_session_ended_keeps_its_assignment_listed() {
        let (jobs, _base) = worker_jobs();

        reported_into_an_ended_session(&jobs).await;

        assert_eq!(
            jobs.running_assignments(),
            vec![RunningAssignment {
                assignment_id: "dispatch-1".into(),
                kind: JobKind::Build,
            }],
            "the report may have died in the socket"
        );
    }

    #[tokio::test]
    async fn an_abort_of_a_reported_job_stops_listing_it() {
        let (jobs, _base) = worker_jobs();
        reported_into_an_ended_session(&jobs).await;

        assert!(!jobs.abort_job("build:1"));

        assert!(jobs.running_assignments().is_empty());
    }

    #[tokio::test]
    async fn a_new_assignment_of_a_reported_job_stops_listing_the_old_one() {
        let (jobs, _base) = worker_jobs();
        reported_into_an_ended_session(&jobs).await;

        jobs.register(
            "build:1".to_owned(),
            JobKind::Build,
            "dispatch-2".to_owned(),
            None,
        );

        assert_eq!(
            jobs.running_assignments(),
            vec![RunningAssignment {
                assignment_id: "dispatch-2".into(),
                kind: JobKind::Build,
            }]
        );
    }

    #[test]
    fn a_channel_reaches_its_job_and_unknown_jobs_are_refused() {
        let (mut jobs, _done_rx) = registry();
        let mut a = jobs.register(
            "exec:a".to_owned(),
            JobKind::Exec,
            "d-a".to_owned(),
            None,
            log(),
        );
        let mut b = jobs.register(
            "exec:b".to_owned(),
            JobKind::Exec,
            "d-b".to_owned(),
            None,
            log(),
        );
        let data = |channel_id| GuestChannelCommand::Data {
            channel_id,
            data: b"ls
"
            .to_vec(),
        };

        assert!(jobs.open_guest_channel("exec:b", 9, GuestChannelKind::Console));
        assert_eq!(
            b.guest_channels.try_recv().ok(),
            Some(GuestChannelCommand::Open {
                channel_id: 9,
                kind: GuestChannelKind::Console
            })
        );
        assert!(a.guest_channels.try_recv().is_err());
        assert!(jobs.route_guest_channel(data(9)));
        assert_eq!(b.guest_channels.try_recv().ok(), Some(data(9)));
        assert!(!jobs.open_guest_channel("exec:x", 10, GuestChannelKind::Console));
        assert!(!jobs.route_guest_channel(data(10)));
    }

    #[tokio::test]
    async fn a_session_end_closes_the_channels_of_every_exec_job() {
        let (jobs, _base) = worker_jobs();
        let mut a = jobs.register("exec:a".to_owned(), JobKind::Exec, "d-a".to_owned(), None);
        let mut b = jobs.register("exec:b".to_owned(), JobKind::Exec, "d-b".to_owned(), None);

        jobs.end_session(RUNNING_JOBS_SINCE);

        assert_eq!(
            a.guest_channels.try_recv().ok(),
            Some(GuestChannelCommand::CloseAll)
        );
        assert_eq!(
            b.guest_channels.try_recv().ok(),
            Some(GuestChannelCommand::CloseAll)
        );
        assert!(!a.abort.is_aborted() && !b.abort.is_aborted());
    }

    #[test]
    fn logs_of_resumed_jobs_survive_the_start_cleanup() {
        let base = tempfile::tempdir().unwrap();
        let config = WorkerConfig {
            base_dir: base.path().to_owned(),
            ..WorkerConfig::default()
        };
        let dir = config.job_log_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for file in [
            "exec_1.log",
            "exec_1.accepted",
            "build_2.log",
            "build_2.offset",
        ] {
            std::fs::write(dir.join(file), "x").unwrap();
        }

        clear_job_logs(&config, &["exec:1".to_owned()]).unwrap();

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["exec_1.accepted", "exec_1.log"]);
    }

    #[tokio::test]
    async fn a_job_of_a_dropped_worker_gives_up_its_buffered_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open(dir.path(), "build:1", JOB_LOG_BUFFER_CAP);
        log.send(&ReconnectingWriter::new(), b"unsent".to_vec())
            .await
            .unwrap();
        assert!(dir.path().join("build_1.log").exists());
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        drop(done_rx);

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            report_when_drained(log, done_tx, ("build:1".to_owned(), Ok(()))),
        )
        .await
        .expect("nobody can replay the log once the worker is gone");
        assert!(!dir.path().join("build_1.log").exists());
    }
}
