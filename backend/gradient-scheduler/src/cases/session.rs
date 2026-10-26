/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gradient_entity::dispatched_job::{self, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, ProjectId};
use gradient_wire::types::{
    BuildFailureKind, BuildMetrics, BuildOutput, BumpedInputWire, CandidateScore,
    DiscoveredDerivation, EvalMessageLevel, EvalStatsReport, InputOutcome, JobKind, JobPhaseSpan,
};

use super::aliases;
use super::shapes::{Profile, eval_key, w_all};
use super::{SchedulerCase, Session, SessionJob, lock, new_port};
use crate::ReportedTimeline;
use crate::actor::{Offer, SchedulerMsg};
use crate::job_handlers::timeline::failed_job_outcome;
use crate::jobs::{Assignment, PendingJob, Reattached, build_job_key};
use crate::test_jobs;

const BUILD_PREFIX: &str = "build:";
const EVAL_PREFIX: &str = "eval:";

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    kind: BuildFailureKind,
    error: String,
    missing_paths: Vec<String>,
    spans: Vec<JobPhaseSpan>,
    elapsed_ms: u64,
    metrics: Option<BuildMetrics>,
}

pub(crate) fn failure(kind: BuildFailureKind) -> Failure {
    Failure {
        kind,
        error: String::new(),
        missing_paths: Vec::new(),
        spans: Vec::new(),
        elapsed_ms: 0,
        metrics: None,
    }
}

impl Failure {
    pub fn error(mut self, error: &str) -> Self {
        self.error = error.into();
        self
    }

    pub fn missing(mut self, paths: &[String]) -> Self {
        self.missing_paths = paths.to_vec();
        self
    }

    pub fn spans(mut self, spans: Vec<JobPhaseSpan>) -> Self {
        self.spans = spans;
        self
    }

    pub fn elapsed_ms(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    pub fn metrics(mut self, metrics: BuildMetrics) -> Self {
        self.metrics = Some(metrics);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClusterAssignment {
    pub assignment_id: DispatchedJobId,
    pub attempt: String,
    pub role: String,
    pub index: u32,
    pub prepare_secs: u32,
}

impl SchedulerCase {
    pub async fn connect(&self, worker: &str, profile: Profile, projects: &[ProjectId]) {
        self.register(worker, profile, projects, false).await;
    }

    pub async fn connect_reclaiming(&self, worker: &str, profile: Profile, projects: &[ProjectId]) {
        self.register(worker, profile, projects, true).await;
    }

    pub async fn requeue_unreclaimed_after(&self, elapsed: Duration) {
        self.scheduler
            .requeue_unreclaimed(Instant::now() + elapsed)
            .await;
    }

    async fn register(
        &self,
        worker: &str,
        profile: Profile,
        projects: &[ProjectId],
        reclaims_jobs: bool,
    ) {
        let projects: HashSet<ProjectId> = projects.iter().copied().collect();
        let port = self.port_of(worker);
        let registered = self
            .scheduler
            .register_worker(
                worker,
                profile.caps.clone(),
                projects.clone(),
                port,
                reclaims_jobs,
            )
            .await
            .expect("the worker registers");
        if let Some(hardware) = profile.hardware {
            self.scheduler
                .update_worker_capabilities(worker, hardware)
                .await;
        }
        if let Some(session) = lock(&self.sessions).get_mut(worker) {
            session.caps = profile.caps;
            session.projects = projects;
            session.last_seen = registered.last_seen;
        }
        lock(&self.connected_workers).insert(worker.to_owned());
        lock(&self.connected_on_any_instance).insert(worker.to_owned());
    }

    fn port_of(
        &self,
        worker: &str,
    ) -> std::sync::Arc<dyn gradient_pool::session_port::SessionPort> {
        let mut sessions = lock(&self.sessions);
        let session = sessions.entry(worker.to_owned()).or_insert_with(|| {
            let (port, inbox) = new_port();
            Session {
                port,
                inbox,
                caps: Default::default(),
                projects: HashSet::new(),
                last_seen: Default::default(),
                jobs: HashMap::new(),
            }
        });
        std::sync::Arc::clone(&session.port)
    }

    pub async fn capabilities(&self, worker: &str, profile: Profile) {
        self.scheduler
            .update_worker_capabilities(worker, profile.hardware.unwrap_or_default())
            .await;
    }

    pub async fn disconnect(&self, worker: &str) {
        self.scheduler.unregister_worker(worker).await;
        if let Some(session) = lock(&self.sessions).get_mut(worker) {
            session.jobs.clear();
        }
    }

    pub fn heartbeat(&self, worker: &str) {
        let now = gradient_types::now().and_utc().timestamp_millis();
        if let Some(session) = lock(&self.sessions).get(worker) {
            session.last_seen.store(now, Ordering::Relaxed);
        }
    }

    pub async fn draining(&self, worker: &str) {
        self.scheduler.mark_worker_draining(worker).await;
    }

    pub async fn authorize(&self, worker: &str, projects: &[ProjectId]) {
        let projects: HashSet<ProjectId> = projects.iter().copied().collect();
        self.scheduler
            .update_authorized_peers(worker, projects.clone())
            .await;
        if let Some(session) = lock(&self.sessions).get_mut(worker) {
            session.projects = projects;
        }
    }

    pub async fn reauth_result(&self, worker: &str, projects: &[ProjectId]) {
        self.authorize(worker, projects).await;
    }

    pub async fn revoke(&self, worker: &str, projects: &[ProjectId]) {
        let revoked: HashSet<ProjectId> = projects.iter().copied().collect();
        self.scheduler
            .abort_project_jobs_on_worker(worker, &revoked)
            .await;
        let remaining: Vec<ProjectId> = lock(&self.sessions)
            .get(worker)
            .map(|s| s.projects.difference(&revoked).copied().collect())
            .unwrap_or_default();
        self.authorize(worker, &remaining).await;
    }

    pub async fn request_reauth(&self, worker: &str) {
        self.scheduler.request_reauth(worker).await;
        self.scheduler.counts().await;
    }

    pub async fn offers_full(&self, worker: &str) -> Offer {
        self.offers(worker, false).await
    }

    pub async fn offers_delta(&self, worker: &str) -> Offer {
        self.offers(worker, true).await
    }

    async fn offers(&self, worker: &str, only_new: bool) -> Offer {
        let worker = worker.to_owned();
        self.scheduler
            .call(|reply| SchedulerMsg::Candidates {
                worker,
                only_new,
                reply,
            })
            .await
            .expect("the scheduler core lists the candidates")
    }

    pub async fn score_all(&self, worker: &str) {
        let mut offered: Vec<String> = self.sent_set(worker).await.into_iter().collect();
        offered.sort();
        let scores = offered
            .iter()
            .map(|key| (key.as_str(), 0, 0, false))
            .collect::<Vec<_>>();
        self.scores(worker, &scores).await;
    }

    pub async fn scores(&self, worker: &str, scores: &[(&str, u32, u64, bool)]) {
        let scores = scores
            .iter()
            .map(|(key, missing, missing_nar_size, outputs_present)| {
                aliases::candidate_score(key, *missing, *missing_nar_size, *outputs_present)
            })
            .collect::<Vec<CandidateScore>>();
        aliases::store_scores(&self.scheduler, worker, scores).await;
    }

    pub async fn request_job(&self, worker: &str, kind: JobKind) -> Option<Assignment> {
        let assignment = self.scheduler.request_job(worker, kind).await?;
        self.remember(
            worker,
            assignment.job_id(),
            assignment.assignment_id(),
            assignment.pending.clone(),
        );
        Some(assignment)
    }

    pub async fn idle(&self, worker: &str, kind: JobKind) {
        self.offers_full(worker).await;
        self.score_all(worker).await;
        let assignment = self.request_job(worker, kind).await;
        assert!(
            assignment.is_none(),
            "{worker} idles only while nothing is handed to it, got {:?}",
            assignment.map(|a| a.job_id().to_owned())
        );
    }

    fn remember(&self, worker: &str, key: &str, assignment_id: DispatchedJobId, job: PendingJob) {
        if let Some(session) = lock(&self.sessions).get_mut(worker) {
            session
                .jobs
                .insert(key.to_owned(), SessionJob { assignment_id, job });
        }
    }

    fn forget(&self, worker: &str, key: &str) -> Option<SessionJob> {
        lock(&self.sessions)
            .get_mut(worker)
            .and_then(|s| s.jobs.remove(key))
    }

    async fn owns_remembered(&self, worker: &str, key: &str) -> bool {
        let remembered = lock(&self.sessions)
            .get(worker)
            .and_then(|s| s.jobs.get(key))
            .map(|j| j.assignment_id);
        match remembered {
            Some(assignment) => self.scheduler.owns_dispatch(worker, key, assignment).await,
            None => false,
        }
    }

    pub async fn accept(&self, _worker: &str, key: &str) {
        self.scheduler.cluster_member_accepted(key).await;
    }

    pub async fn reject(&self, worker: &str, key: &str) -> bool {
        if let Some(job) = self.forget(worker, key) {
            aliases::abandon_row(&self.scheduler.db.worker_db, job.assignment_id).await;
        }
        let member = self.scheduler.cluster_member_rejected(key).await;
        if !member {
            self.scheduler.job_rejected(worker, key).await;
        }
        member
    }

    pub async fn take_cluster_assignment(&self, worker: &str, key: &str) -> ClusterAssignment {
        let prepared = self
            .scheduler
            .cluster
            .prepared
            .take(key)
            .await
            .unwrap_or_else(|| panic!("no prepared cluster member {key}"));
        let assignment = &prepared.assignment;
        self.remember(
            worker,
            key,
            assignment.assignment_id(),
            assignment.pending.clone(),
        );
        ClusterAssignment {
            assignment_id: assignment.assignment_id(),
            attempt: prepared.membership.attempt.clone(),
            role: prepared.membership.role.clone(),
            index: prepared.membership.index,
            prepare_secs: aliases::prepare_secs(&prepared.membership),
        }
    }

    pub async fn report_building(&self, worker: &str, key: &str) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        self.scheduler
            .handle_build_status_update(build_id(key), worker)
            .await;
    }

    pub async fn report_build_output(
        &self,
        worker: &str,
        key: &str,
        outputs: Vec<BuildOutput>,
        substituted: bool,
        metrics: Option<BuildMetrics>,
    ) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        let _ = self
            .scheduler
            .handle_build_output(key, build_id(key), outputs, metrics, substituted)
            .await;
    }

    pub async fn complete(
        &self,
        worker: &str,
        key: &str,
        assignment: DispatchedJobId,
        spans: Vec<JobPhaseSpan>,
        elapsed_ms: u64,
    ) {
        self.forget(worker, key);
        if !self.scheduler.owns_dispatch(worker, key, assignment).await {
            return;
        }
        self.scheduler
            .close_job_timeline(
                assignment,
                DispatchedJobOutcome::Completed,
                ReportedTimeline::received(spans, elapsed_ms),
            )
            .await;
        let _ = self.scheduler.handle_job_completed(worker, key).await;
    }

    pub async fn fail(
        &self,
        worker: &str,
        key: &str,
        assignment: DispatchedJobId,
        failure: Failure,
    ) {
        self.forget(worker, key);
        if !self.scheduler.owns_dispatch(worker, key, assignment).await
            || self.scheduler.cluster_member_released(key).await
        {
            return;
        }
        let _ = self
            .scheduler
            .persist_job_timeline(
                assignment,
                failed_job_outcome(failure.kind),
                ReportedTimeline::received(failure.spans, failure.elapsed_ms),
            )
            .await;
        let _ = self
            .scheduler
            .handle_job_failed(
                worker,
                key,
                &failure.error,
                failure.kind,
                &failure.missing_paths,
                failure.metrics,
            )
            .await;
    }

    pub async fn evalrun(&self, worker: &str, evaluation: EvaluationId) -> DispatchedJobId {
        if !lock(&self.connected_workers).contains(worker) {
            self.connect(worker, w_all(), &[]).await;
        }
        self.assign_queued_evals().await;
        self.offers_full(worker).await;
        self.score_all(worker).await;
        let assignment = self
            .request_job(worker, JobKind::Flake)
            .await
            .unwrap_or_else(|| panic!("{worker} gets the eval job of {evaluation}"));
        assert_eq!(
            assignment.job_id(),
            eval_key(evaluation),
            "the evaluation run hands {worker} the earliest queued evaluation"
        );
        assignment.assignment_id()
    }

    pub async fn report_status(&self, worker: &str, key: &str, status: EvaluationStatus) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        self.scheduler.handle_eval_status_update(key, status).await;
    }

    pub async fn advance_evaluation(&self, worker: &str, key: &str, to: EvaluationStatus) {
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            self.report_status(worker, key, status).await;
            if status == to {
                return;
            }
        }
    }

    pub async fn report_flake_source(&self, worker: &str, key: &str, path: &str) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        self.scheduler
            .persist_flake_source(key, Some(path.to_owned()))
            .await;
    }

    pub async fn report_eval_result(
        &self,
        worker: &str,
        key: &str,
        derivations: Vec<DiscoveredDerivation>,
    ) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        let _ = self
            .scheduler
            .handle_eval_result(key, derivations, Vec::new(), Vec::new())
            .await;
    }

    pub async fn report_eval_message(
        &self,
        _worker: &str,
        key: &str,
        level: EvalMessageLevel,
        source: &str,
        message: &str,
    ) {
        let _ = aliases::store_eval_message(
            &self.scheduler,
            key,
            level,
            source.to_owned(),
            message.to_owned(),
        )
        .await;
    }

    pub async fn report_eval_stats(&self, worker: &str, key: &str, stats: EvalStatsReport) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        let _ = aliases::store_eval_stats(&self.scheduler, worker, key, stats).await;
    }

    pub async fn report_input_update_result(
        &self,
        worker: &str,
        key: &str,
        candidate_lock: &str,
        bumped: Vec<BumpedInputWire>,
    ) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        self.scheduler
            .persist_input_update_result(key, candidate_lock.to_owned(), bumped)
            .await;
    }

    pub async fn report_input_update_expansion(&self, worker: &str, key: &str, matched: &[&str]) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        self.scheduler
            .persist_input_update_expansion(key, matched.iter().map(|m| (*m).to_owned()).collect())
            .await;
    }

    pub async fn import_request(
        &self,
        worker: &str,
        key: &str,
        request_id: &str,
        paths: &[String],
    ) {
        let evaluation: EvaluationId = key
            .strip_prefix(EVAL_PREFIX)
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("{key} is no eval job key"));
        if !self.owns_remembered(worker, key).await {
            return;
        }
        let _ = self
            .scheduler
            .handle_import_request(
                worker,
                key,
                evaluation,
                request_id.to_owned(),
                paths.to_vec(),
            )
            .await;
    }

    pub async fn input_request(&self, worker: &str, key: &str, request_id: &str, request: &str) {
        if !self.owns_remembered(worker, key).await {
            return;
        }
        let _ = self
            .scheduler
            .handle_input_request(worker, key, request_id.to_owned(), request.to_owned())
            .await;
    }

    pub async fn input_fetched(&self, worker: &str, fetch_id: &str, outcome: InputOutcome) {
        self.scheduler
            .handle_input_fetched(worker, fetch_id, outcome)
            .await;
    }

    pub async fn input_fetch_progress(
        &self,
        worker: &str,
        fetch_id: &str,
        bytes_done: u64,
        bytes_total: Option<u64>,
    ) {
        self.scheduler
            .handle_input_fetch_progress(worker, fetch_id, bytes_done, bytes_total)
            .await;
    }

    pub async fn respawn_core(&self) {
        let old = lock(&self.core).clone();
        old.stop_and_wait(None, None)
            .await
            .expect("the scheduler core stops");
        let new = self
            .scheduler
            .spawn_core(None)
            .await
            .expect("the scheduler core starts again");
        *lock(&self.core) = new;
    }

    pub async fn reattach(&self, worker: &str, jobs: &[(&str, DispatchedJobId)]) {
        let mut active = Vec::new();
        for (key, assignment_id) in jobs {
            let job = match self.remembered(worker, key, *assignment_id) {
                Some(job) => job,
                None => self.job_of_row(key, *assignment_id).await,
            };
            self.remember(worker, key, *assignment_id, job.clone());
            active.push(Reattached::single((*key).to_owned(), *assignment_id, job));
        }
        let (caps, projects, port) = {
            let sessions = lock(&self.sessions);
            let session = sessions
                .get(worker)
                .unwrap_or_else(|| panic!("connect {worker} first"));
            (
                session.caps.clone(),
                session.projects.clone(),
                std::sync::Arc::clone(&session.port),
            )
        };
        let registered = self
            .scheduler
            .reattach_worker(worker, caps, projects, port, active, false)
            .await
            .expect("the worker reattaches");
        if let Some(session) = lock(&self.sessions).get_mut(worker) {
            session.last_seen = registered.last_seen;
        }
    }

    fn remembered(
        &self,
        worker: &str,
        key: &str,
        assignment_id: DispatchedJobId,
    ) -> Option<PendingJob> {
        lock(&self.sessions)
            .get(worker)
            .and_then(|s| s.jobs.get(key))
            .filter(|j| j.assignment_id == assignment_id)
            .map(|j| j.job.clone())
    }

    async fn job_of_row(&self, key: &str, assignment_id: DispatchedJobId) -> PendingJob {
        let row = self
            .db
            .find::<dispatched_job::Entity>(assignment_id)
            .await
            .unwrap_or_else(|| panic!("no dispatched_job row {assignment_id}"));
        if let Some(id) = key.strip_prefix(BUILD_PREFIX) {
            let shared_build = id.parse().expect("a shared build id");
            let build = self
                .db
                .find::<gradient_entity::derivation_build::Entity>(shared_build)
                .await
                .expect("the shared build of the row");
            let mut job = test_jobs::build_job(row.evaluation_id, row.project, shared_build);
            job.derivation = build.derivation;
            assert_eq!(build_job_key(shared_build), key);
            return PendingJob::Build(job);
        }
        let mut job = test_jobs::eval_job(row.project);
        job.evaluation_id = row.evaluation_id;
        job.task_id = row.task;
        PendingJob::Eval(job)
    }
}

fn build_id(key: &str) -> &str {
    key.strip_prefix(BUILD_PREFIX)
        .unwrap_or_else(|| panic!("{key} is no build job key"))
}
