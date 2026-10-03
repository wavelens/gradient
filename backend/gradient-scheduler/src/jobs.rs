/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use gradient_entity::dispatched_job::DispatchedJobKind;
use gradient_entity::evaluation::WalkMode;
use gradient_types::ids::{
    ClusterAttemptId, ClusterJobId, CommitId, DerivationBuildId, DerivationId, DispatchedJobId,
    EvaluationId, ProjectId, TaskId,
};
use gradient_wire::types::{
    BuildJob, CandidateScore, FlakeJob, FlakeSource, FlakeStep, Job, JobCandidate, JobKind,
    RequiredPath,
};

use crate::cluster::{ClusterBook, PendingCluster};
use gradient_pool::WorkerCaps;
use gradient_pool::score::{JobContext, ScoredJob, ScoringPolicy, WorkerContext};

#[derive(Debug, Clone)]
pub struct PendingEvalJob {
    pub evaluation_id: EvaluationId,
    pub task_id: Option<TaskId>,
    pub project_id: ProjectId,
    pub commit_id: CommitId,
    pub repository: String,
    pub job: FlakeJob,
    pub required_paths: Vec<RequiredPath>,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    pub rescore_count: u32,
    pub history: gradient_pool::score::HistoryPrediction,
    pub walk_mode: WalkMode,
    pub prioritized: bool,
}

impl PendingEvalJob {
    pub fn cached_followup(&self, store_path: String) -> PendingEvalJob {
        let mut follow = self.clone();
        follow.job.steps = vec![FlakeStep::EvaluateFlake, FlakeStep::EvaluateDerivations];
        follow.job.source = FlakeSource::Cached {
            store_path: store_path.clone(),
        };
        follow.required_paths = vec![RequiredPath {
            path: store_path,
            cache_info: None,
        }];
        follow
    }
}

#[derive(Debug, Clone)]
pub struct PendingBuildJob {
    pub derivation_build: DerivationBuildId,
    pub derivation: DerivationId,
    pub evaluation_id: EvaluationId,
    pub project_id: ProjectId,
    pub job: BuildJob,
    pub required_paths: Vec<RequiredPath>,
    pub dependency_count: u32,
    pub closure_size: Option<i64>,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub history: gradient_pool::score::HistoryPrediction,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    pub rescore_count: u32,
    pub prioritized: bool,
    pub pname: Option<String>,
    pub substitute: bool,
}

#[derive(Debug, Clone)]
pub enum PendingJob {
    Eval(PendingEvalJob),
    Build(PendingBuildJob),
}

pub fn eval_job_key(evaluation: EvaluationId) -> String {
    format!(
        "{}{evaluation}",
        gradient_db::scheduling::assignment_record::EVAL_KEY_PREFIX
    )
}

pub fn build_job_key(shared_build: DerivationBuildId) -> String {
    format!(
        "{}{shared_build}",
        gradient_db::scheduling::assignment_record::BUILD_KEY_PREFIX
    )
}

impl PendingJob {
    pub fn job_key(&self) -> String {
        match self {
            PendingJob::Eval(j) => eval_job_key(j.evaluation_id),
            PendingJob::Build(j) => build_job_key(j.derivation_build),
        }
    }

    pub fn required_paths(&self) -> &[RequiredPath] {
        match self {
            PendingJob::Eval(j) => &j.required_paths,
            PendingJob::Build(j) => &j.required_paths,
        }
    }

    pub fn prunes_walk(&self) -> bool {
        matches!(self, PendingJob::Eval(j) if j.walk_mode == WalkMode::Pruned)
    }

    pub fn project_id(&self) -> ProjectId {
        match self {
            PendingJob::Eval(j) => j.project_id,
            PendingJob::Build(j) => j.project_id,
        }
    }

    pub fn as_candidate(&self, job_id: &str) -> JobCandidate {
        let (builds, requirement) = match self {
            PendingJob::Build(j) => (j.job.builds.as_slice(), Some(j.job.requirement.clone())),
            PendingJob::Eval(_) => (&[][..], None),
        };
        JobCandidate {
            job_id: job_id.to_owned(),
            required_paths: self.required_paths().to_vec(),
            drv_paths: builds.iter().map(|t| t.drv_path.clone()).collect(),
            output_paths: builds
                .iter()
                .flat_map(|t| &t.outputs)
                .filter(|o| !o.path.is_empty())
                .map(|o| o.path.clone())
                .collect(),
            requirement,
        }
    }

    pub(crate) fn into_job(self) -> Job {
        match self {
            PendingJob::Eval(j) => Job::Flake(j.job),
            PendingJob::Build(j) => Job::Build(j.job),
        }
    }

    pub fn evaluation_id(&self) -> EvaluationId {
        match self {
            PendingJob::Eval(j) => j.evaluation_id,
            PendingJob::Build(j) => j.evaluation_id,
        }
    }

    pub fn kind_disc(&self) -> DispatchedJobKind {
        match self {
            PendingJob::Eval(_) => DispatchedJobKind::Eval,
            PendingJob::Build(_) => DispatchedJobKind::Build,
        }
    }

    pub fn derivation_build(&self) -> Option<DerivationBuildId> {
        match self {
            PendingJob::Build(j) => Some(j.derivation_build),
            PendingJob::Eval(_) => None,
        }
    }

    pub fn pname(&self) -> Option<String> {
        match self {
            PendingJob::Build(j) => j.pname.clone(),
            PendingJob::Eval(_) => None,
        }
    }

    pub fn dependency_count(&self) -> u32 {
        match self {
            PendingJob::Build(j) => j.dependency_count,
            PendingJob::Eval(_) => 0,
        }
    }

    pub fn queued_at(&self) -> chrono::NaiveDateTime {
        match self {
            PendingJob::Build(j) => j.queued_at,
            PendingJob::Eval(j) => j.queued_at,
        }
    }

    pub fn ready_at(&self) -> chrono::NaiveDateTime {
        match self {
            PendingJob::Build(j) => j.ready_at,
            PendingJob::Eval(j) => j.ready_at,
        }
    }

    pub fn rescore_count(&self) -> u32 {
        match self {
            PendingJob::Build(j) => j.rescore_count,
            PendingJob::Eval(j) => j.rescore_count,
        }
    }

    pub fn prioritized(&self) -> bool {
        match self {
            PendingJob::Build(j) => j.prioritized,
            PendingJob::Eval(j) => j.prioritized,
        }
    }

    pub fn set_rescore_count(&mut self, n: u32) {
        match self {
            PendingJob::Build(j) => j.rescore_count = n,
            PendingJob::Eval(j) => j.rescore_count = n,
        }
    }
}

pub struct Assignment {
    pub job: Job,
    pub project_id: ProjectId,
    pub assignment_record: AssignmentRecord,
    pub pending: PendingJob,
}

impl Assignment {
    pub fn job_id(&self) -> &str {
        &self.assignment_record.job_id
    }

    pub fn assignment_id(&self) -> DispatchedJobId {
        self.assignment_record.assignment_id
    }
}

#[derive(Debug, Clone)]
pub struct AssignmentRecord {
    pub job_id: String,
    pub assignment_id: DispatchedJobId,
    pub kind: DispatchedJobKind,
    pub derivation_build: Option<DerivationBuildId>,
    pub evaluation_id: EvaluationId,
    pub project: ProjectId,
    pub task: Option<TaskId>,
    pub score: f64,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    pub score_breakdown: serde_json::Value,
    pub worker_context: serde_json::Value,
    pub job_context: serde_json::Value,
    pub instance_context: serde_json::Value,
    pub substitute: bool,
    pub build_context: serde_json::Value,
}

struct ScoredCandidate {
    total: f64,
    vetoed: bool,
    score_breakdown: serde_json::Value,
    job_context: serde_json::Value,
}

fn job_eligible_for_caps(job: &PendingJob, caps: Option<&WorkerCaps>) -> bool {
    match (job, caps) {
        (_, None) => true,
        (PendingJob::Eval(j), Some(c)) => c.can_eval(&j.job),
        (PendingJob::Build(j), Some(c)) => c.can_build(
            &j.job.requirement.architecture,
            &j.job.requirement.required_features,
        ),
    }
}

pub(crate) fn visible_to(
    job: &PendingJob,
    authorized: Option<&HashSet<ProjectId>>,
    caps: Option<&WorkerCaps>,
) -> bool {
    authorized.is_none_or(|peers| peers.contains(&job.project_id()))
        && job_eligible_for_caps(job, caps)
}

fn wins(sc: &ScoredCandidate) -> bool {
    !sc.vetoed && sc.total >= gradient_pool::score::weights::ASSIGN_FLOOR
}

fn worker_context_of(caps: Option<&WorkerCaps>) -> WorkerContext<'_> {
    match caps {
        Some(c) => WorkerContext {
            architectures: &c.architectures,
            system_features: &c.system_features,
            fetch: c.fetch,
            metrics: c.metrics,
        },
        None => WorkerContext {
            architectures: &[],
            system_features: &[],
            fetch: false,
            metrics: None,
        },
    }
}

fn assignment_record_for(
    job_id: &str,
    job: &PendingJob,
    assignment_id: DispatchedJobId,
    sc: &ScoredCandidate,
    worker_context: serde_json::Value,
    instance_context: serde_json::Value,
) -> AssignmentRecord {
    let (kind_disc, derivation_build, task) = match job {
        PendingJob::Build(b) => (DispatchedJobKind::Build, Some(b.derivation_build), None),
        PendingJob::Eval(e) => (DispatchedJobKind::Eval, None, e.task_id),
    };
    AssignmentRecord {
        job_id: job_id.to_owned(),
        assignment_id,
        kind: kind_disc,
        derivation_build,
        evaluation_id: job.evaluation_id(),
        project: job.project_id(),
        task,
        score: sc.total,
        queued_at: job.queued_at(),
        ready_at: job.ready_at(),
        score_breakdown: sc.score_breakdown.clone(),
        worker_context,
        job_context: sc.job_context.clone(),
        instance_context,
        substitute: matches!(job, PendingJob::Build(b) if b.substitute),
        build_context: match job {
            PendingJob::Build(b) => serde_json::json!({
                "architecture": b.job.requirement.architecture,
                "required_features": b.job.requirement.required_features,
                "dependency_count": b.dependency_count,
                "closure_size": b.closure_size,
                "prefer_local_build": b.prefer_local_build,
                "is_fixed_output": b.is_fixed_output,
                "substitute": b.substitute,
            }),
            PendingJob::Eval(_) => serde_json::json!({}),
        },
    }
}

struct ProjectWorkShare {
    by_project: HashMap<ProjectId, f64>,
    total: f64,
}

impl ProjectWorkShare {
    fn share(&self, project: ProjectId) -> Option<f32> {
        (self.total > 0.0)
            .then(|| (self.by_project.get(&project).copied().unwrap_or(0.0) / self.total) as f32)
    }
}

pub fn is_fetch_only(job: &FlakeJob) -> bool {
    job.steps.as_slice() == [FlakeStep::FetchFlake]
}

pub(crate) fn is_fetch_only_job(job: &PendingJob) -> bool {
    matches!(job, PendingJob::Eval(j) if is_fetch_only(&j.job))
}

#[derive(Debug, Clone, Default)]
pub struct WorkerJobScore {
    pub missing_count: u32,
    pub missing_nar_size: u64,
    pub outputs_present: bool,
}

#[derive(Debug, Clone)]
pub struct PendingJobInfo {
    pub kind: DispatchedJobKind,
    pub evaluation_id: EvaluationId,
    pub derivation_build: Option<DerivationBuildId>,
    pub project: ProjectId,
    pub queued_at: chrono::NaiveDateTime,
    pub dependency_count: u32,
    pub pname: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BoardActiveJob {
    pub worker_id: String,
    pub project: ProjectId,
    pub kind: DispatchedJobKind,
    pub architecture: Option<String>,
    pub required_features: Vec<String>,
    pub fetch_step: bool,
    pub eval_step: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionCandidate {
    pub id: DispatchedJobId,
    pub job_id: String,
    pub kind: i16,
    pub project: ProjectId,
    pub derivation_build: Option<DerivationBuildId>,
    pub evaluation_id: EvaluationId,
    pub pname: Option<String>,
    pub score: f64,
    pub won: bool,
    pub queued_at: chrono::NaiveDateTime,
    pub score_breakdown: serde_json::Value,
    pub job_context: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AssignDecision {
    pub at: chrono::NaiveDateTime,
    pub worker_id: String,
    pub kind: i16,
    pub winner: Option<String>,
    pub worker_context: serde_json::Value,
    pub instance_context: serde_json::Value,
    pub candidates: Vec<DecisionCandidate>,
}

#[derive(Debug, Clone)]
pub struct CandidateDetail {
    pub id: DispatchedJobId,
    pub kind: i16,
    pub project: ProjectId,
    pub derivation_build: Option<DerivationBuildId>,
    pub evaluation_id: EvaluationId,
    pub pname: Option<String>,
    pub score: f64,
    pub won: bool,
    pub worker_id: String,
    pub scored_at: chrono::NaiveDateTime,
    pub queued_at: chrono::NaiveDateTime,
    pub score_breakdown: serde_json::Value,
    pub worker_context: serde_json::Value,
    pub job_context: serde_json::Value,
    pub instance_context: serde_json::Value,
}

const DECISION_RING_CAP: usize = 200;
const CANDIDATES_PER_DECISION: usize = 64;

#[derive(Debug)]
struct ActiveJob {
    worker: String,
    job: PendingJob,
    aborted_at: Option<Instant>,
    cluster: Option<ClusterAttemptId>,
}

impl ActiveJob {
    fn new(worker: &str, job: PendingJob) -> Self {
        Self {
            worker: worker.to_owned(),
            job,
            aborted_at: None,
            cluster: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Reattached {
    pub job_id: String,
    pub job: PendingJob,
    pub cluster: Option<ClusterAttemptId>,
}

impl Reattached {
    pub fn single(job_id: String, job: PendingJob) -> Self {
        Self {
            job_id,
            job,
            cluster: None,
        }
    }
}

#[derive(Debug)]
pub struct LostMember {
    pub attempt: ClusterAttemptId,
    pub key: String,
    pub job: PendingJob,
}

#[derive(Debug, Default)]
pub struct Disconnected {
    pub requeued: Vec<PendingJob>,
    pub cluster_members: Vec<LostMember>,
}

#[derive(Debug, Default)]
pub struct JobTracker {
    pending: HashMap<String, PendingJob>,
    scores: HashMap<String, HashMap<String, WorkerJobScore>>,
    active: HashMap<String, ActiveJob>,
    decisions: VecDeque<AssignDecision>,
    clusters: ClusterBook,
}

impl JobTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pending(&mut self, job_id: String, job: PendingJob) -> JobCandidate {
        let candidate = job.as_candidate(&job_id);
        // Two dispatch passes can both clear the `contains_job` filter before either one is
        // enqueuing. A job already pending or active is kept as is under the tracker write lock.
        if self.pending.contains_key(&job_id) || self.active.contains_key(&job_id) {
            return candidate;
        }
        self.pending.insert(job_id, job);
        candidate
    }

    pub fn candidates_for_worker(
        &self,
        authorized: Option<&HashSet<ProjectId>>,
        caps: Option<&WorkerCaps>,
    ) -> Vec<JobCandidate> {
        let members = self
            .clusters
            .jobs()
            .filter(|(_, job)| visible_to(job, authorized, caps));
        self.eligible_for_worker(authorized, caps)
            .chain(members)
            .map(|(id, job)| job.as_candidate(id))
            .collect()
    }

    fn eligible_for_worker<'s>(
        &'s self,
        authorized: Option<&'s HashSet<ProjectId>>,
        caps: Option<&'s WorkerCaps>,
    ) -> impl Iterator<Item = (&'s String, &'s PendingJob)> {
        self.pending
            .iter()
            .filter(move |(_, job)| visible_to(job, authorized, caps))
    }

    pub fn record_scores(&mut self, worker_id: &str, scores: Vec<CandidateScore>) {
        let worker_scores = self.scores.entry(worker_id.to_owned()).or_default();
        for score in scores {
            worker_scores.insert(
                score.job_id.clone(),
                WorkerJobScore {
                    missing_count: score.missing_count,
                    missing_nar_size: score.missing_nar_size,
                    outputs_present: score.outputs_present,
                },
            );
        }
    }

    fn push_decision(&mut self, decision: AssignDecision) {
        if self.decisions.len() >= DECISION_RING_CAP {
            self.decisions.pop_front();
        }

        self.decisions.push_back(decision);
    }

    pub fn recent_decisions(&self) -> Vec<AssignDecision> {
        self.decisions.iter().rev().cloned().collect()
    }

    pub fn candidate_detail(&self, id: DispatchedJobId) -> Option<CandidateDetail> {
        for d in self.decisions.iter().rev() {
            if let Some(c) = d.candidates.iter().find(|c| c.id == id) {
                return Some(CandidateDetail {
                    id: c.id,
                    kind: c.kind,
                    project: c.project,
                    derivation_build: c.derivation_build,
                    evaluation_id: c.evaluation_id,
                    pname: c.pname.clone(),
                    score: c.score,
                    won: c.won,
                    worker_id: d.worker_id.clone(),
                    scored_at: d.at,
                    queued_at: c.queued_at,
                    score_breakdown: c.score_breakdown.clone(),
                    worker_context: d.worker_context.clone(),
                    job_context: c.job_context.clone(),
                    instance_context: d.instance_context.clone(),
                });
            }
        }

        None
    }

    pub fn take_best_of_kind(
        &mut self,
        worker_id: &str,
        authorized: Option<&HashSet<ProjectId>>,
        caps: Option<&WorkerCaps>,
        kind: &JobKind,
        policy: &dyn ScoringPolicy,
        instance: &gradient_pool::score::InstanceContext,
    ) -> Option<Assignment> {
        let worker_ctx = worker_context_of(caps);
        let scored = self.score_candidates(
            worker_id,
            authorized,
            caps,
            kind,
            policy,
            instance,
            &worker_ctx,
        );
        let winner_id = scored
            .iter()
            .find(|(_, sc)| wins(sc))
            .map(|(id, _)| id.clone());

        let worker_context = serde_json::to_value(crate::views::WorkerContextView::new(
            &worker_ctx,
            caps.map(|c| c.capabilities.clone()).unwrap_or_default(),
        ))
        .unwrap_or(serde_json::Value::Null);
        let instance_context = serde_json::to_value(instance).unwrap_or(serde_json::Value::Null);

        self.record_decision(
            worker_id,
            kind,
            winner_id.as_deref(),
            &scored,
            &worker_context,
            &instance_context,
        );

        let job_id = winner_id?;
        let (_, winner_sc) = scored
            .into_iter()
            .find(|(id, _)| *id == job_id)
            .expect("the winner is a scored candidate");
        let record = assignment_record_for(
            &job_id,
            self.pending.get(&job_id)?,
            DispatchedJobId::now_v7(),
            &winner_sc,
            worker_context,
            instance_context,
        );

        self.assign_pending(worker_id, &job_id, record)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    fn score_candidates(
        &self,
        worker_id: &str,
        authorized: Option<&HashSet<ProjectId>>,
        caps: Option<&WorkerCaps>,
        kind: &JobKind,
        policy: &dyn ScoringPolicy,
        instance: &gradient_pool::score::InstanceContext,
        worker_ctx: &WorkerContext<'_>,
    ) -> Vec<(String, ScoredCandidate)> {
        let worker_scores = self.scores.get(worker_id);
        let shares = self.project_work_shares(policy, instance);
        let now = gradient_types::now();

        let mut scored: Vec<(String, ScoredCandidate)> = self
            .eligible_for_worker(authorized, caps)
            .filter(|(_, j)| {
                matches!(
                    (kind, j),
                    (JobKind::Flake, PendingJob::Eval(_)) | (JobKind::Build, PendingJob::Build(_))
                )
            })
            .map(|(id, job)| {
                let score = worker_scores.and_then(|ws| ws.get(id));
                let candidate =
                    self.score_job(id, job, score, &shares, policy, instance, worker_ctx, now);
                (id.clone(), candidate)
            })
            .collect();
        scored.sort_by(|(id_a, a), (id_b, b)| {
            b.total
                .partial_cmp(&a.total)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| id_a.cmp(id_b))
        });
        scored
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    fn score_job(
        &self,
        id: &str,
        job: &PendingJob,
        score: Option<&WorkerJobScore>,
        shares: &ProjectWorkShare,
        policy: &dyn ScoringPolicy,
        instance: &gradient_pool::score::InstanceContext,
        worker_ctx: &WorkerContext<'_>,
        now: chrono::NaiveDateTime,
    ) -> ScoredCandidate {
        let scored_job = match job {
            PendingJob::Eval(e) => ScoredJob::new_eval(
                id,
                job.project_id(),
                e.job.steps.contains(&FlakeStep::FetchFlake),
                e.history,
            ),
            PendingJob::Build(b) => ScoredJob::new_build(
                id,
                job.project_id(),
                b.job.requirement.architecture.as_str(),
                b.prefer_local_build,
                b.is_fixed_output,
                b.pname.as_deref(),
                b.closure_size,
                b.history,
            ),
        };
        let ctx = JobContext {
            job: &scored_job,
            missing_count: score.map(|s| s.missing_count),
            missing_nar_size: score.map(|s| s.missing_nar_size),
            outputs_present: score.is_some_and(|s| s.outputs_present),
            dependency_count: job.dependency_count(),
            queued_at: job.queued_at(),
            ready_at: job.ready_at(),
            project_work_share: shares.share(job.project_id()),
            prioritized: job.prioritized(),
            rescore_count: job.rescore_count(),
            now,
        };
        let breakdown = policy.score_detailed(&ctx, worker_ctx, instance);
        ScoredCandidate {
            total: breakdown.total,
            vetoed: !breakdown.vetoes.is_empty(),
            score_breakdown: serde_json::to_value(&breakdown).unwrap_or(serde_json::Value::Null),
            job_context: serde_json::to_value(crate::views::JobContextView::new(&ctx, job))
                .unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn member_record(
        &self,
        worker_id: &str,
        caps: Option<&WorkerCaps>,
        key: &str,
        job: &PendingJob,
        policy: &dyn ScoringPolicy,
        instance: &gradient_pool::score::InstanceContext,
    ) -> AssignmentRecord {
        let worker_ctx = worker_context_of(caps);
        let score = self.scores.get(worker_id).and_then(|ws| ws.get(key));
        let shares = self.project_work_shares(policy, instance);
        let sc = self.score_job(
            key,
            job,
            score,
            &shares,
            policy,
            instance,
            &worker_ctx,
            gradient_types::now(),
        );
        let worker_context = serde_json::to_value(crate::views::WorkerContextView::new(
            &worker_ctx,
            caps.map(|c| c.capabilities.clone()).unwrap_or_default(),
        ))
        .unwrap_or(serde_json::Value::Null);
        let instance_context = serde_json::to_value(instance).unwrap_or(serde_json::Value::Null);

        assignment_record_for(
            key,
            job,
            DispatchedJobId::now_v7(),
            &sc,
            worker_context,
            instance_context,
        )
    }

    fn project_work_shares(
        &self,
        policy: &dyn ScoringPolicy,
        instance: &gradient_pool::score::InstanceContext,
    ) -> ProjectWorkShare {
        let mut by_project: HashMap<ProjectId, f64> = HashMap::new();
        let mut total: f64 = 0.0;
        if policy.uses_project_work_share() {
            for ActiveJob { job, .. } in self.active.values() {
                if let PendingJob::Build(b) = job {
                    let w = if b.history.build_time_ms > 0 {
                        b.history.build_time_ms as f64
                    } else {
                        (if b.prefer_local_build { 0.5 } else { 1.0 })
                            * instance.build_time_ms.w1h.unwrap_or(0.0)
                    };
                    *by_project.entry(b.project_id).or_default() += w;
                    total += w;
                }
            }
        }
        ProjectWorkShare { by_project, total }
    }

    fn record_decision(
        &mut self,
        worker_id: &str,
        kind: &JobKind,
        winner: Option<&str>,
        scored: &[(String, ScoredCandidate)],
        worker_context: &serde_json::Value,
        instance_context: &serde_json::Value,
    ) {
        let candidates: Vec<DecisionCandidate> = scored
            .iter()
            .take(CANDIDATES_PER_DECISION)
            .filter_map(|(id, sc)| {
                self.pending.get(id).map(|job| DecisionCandidate {
                    id: DispatchedJobId::now_v7(),
                    job_id: id.clone(),
                    kind: i16::from(job.kind_disc()),
                    project: job.project_id(),
                    derivation_build: job.derivation_build(),
                    evaluation_id: job.evaluation_id(),
                    pname: job.pname(),
                    score: sc.total,
                    won: winner == Some(id.as_str()),
                    queued_at: job.queued_at(),
                    score_breakdown: sc.score_breakdown.clone(),
                    job_context: sc.job_context.clone(),
                })
            })
            .collect();
        if candidates.is_empty() {
            return;
        }

        self.push_decision(AssignDecision {
            at: gradient_types::now(),
            worker_id: worker_id.to_owned(),
            kind: i16::from(match kind {
                JobKind::Flake => DispatchedJobKind::Eval,
                JobKind::Build => DispatchedJobKind::Build,
            }),
            winner: winner.map(str::to_owned),
            worker_context: worker_context.clone(),
            instance_context: instance_context.clone(),
            candidates,
        });
    }

    fn assign_pending(
        &mut self,
        worker_id: &str,
        job_id: &str,
        record: AssignmentRecord,
    ) -> Option<Assignment> {
        let job = self.pending.remove(job_id)?;
        if let Some(ws) = self.scores.get_mut(worker_id) {
            ws.remove(job_id);
        }

        let assignment = Assignment {
            job: job.clone().into_job(),
            project_id: job.project_id(),
            assignment_record: record,
            pending: job.clone(),
        };
        self.active
            .insert(job_id.to_owned(), ActiveJob::new(worker_id, job));
        Some(assignment)
    }

    pub fn restore_active(&mut self, worker_id: &str, reattached: Reattached) {
        let Reattached {
            job_id,
            job,
            cluster,
        } = reattached;
        if self.active.contains_key(&job_id) {
            return;
        }
        self.pending.remove(&job_id);
        self.clusters.forget(&job_id);
        self.active.insert(
            job_id,
            ActiveJob {
                cluster,
                ..ActiveJob::new(worker_id, job)
            },
        );
    }

    pub fn release_to_pending(&mut self, job_id: &str) {
        if let Some(active) = self.active.remove(job_id)
            && active.cluster.is_none()
        {
            self.pending.insert(job_id.to_owned(), active.job);
        }
    }

    pub fn add_member(
        &mut self,
        of: gradient_db::scheduling::cluster::MemberOf,
        key: String,
        job: PendingJob,
    ) {
        if self.active.contains_key(&key) {
            return;
        }
        self.clusters.add(of, key, job);
    }

    pub fn ready_clusters(&self) -> impl Iterator<Item = &PendingCluster> {
        self.clusters.ready()
    }

    pub fn drop_cluster(&mut self, id: ClusterJobId) -> Option<PendingCluster> {
        let cluster = self.clusters.drop_cluster(id)?;
        for member in &cluster.members {
            self.forget_job_scores(&member.key);
        }
        Some(cluster)
    }

    pub fn is_aborting(&self, job_id: &str) -> bool {
        self.active
            .get(job_id)
            .is_some_and(|a| a.aborted_at.is_some())
    }

    pub fn waiting_cluster(&self, id: ClusterJobId) -> Option<&PendingCluster> {
        self.clusters.get(id)
    }

    pub fn take_cluster(&mut self, id: ClusterJobId) -> Option<PendingCluster> {
        self.clusters.take(id)
    }

    pub fn restore_cluster(&mut self, cluster: PendingCluster) {
        self.clusters.restore(cluster);
    }

    pub fn activate_members(
        &mut self,
        attempt: ClusterAttemptId,
        members: Vec<(String, String, PendingJob)>,
    ) {
        for (worker, key, job) in members {
            self.forget_job_scores(&key);
            self.clusters.release(&key);
            self.active.insert(
                key,
                ActiveJob {
                    cluster: Some(attempt),
                    ..ActiveJob::new(&worker, job)
                },
            );
        }
    }

    pub fn active_cluster(&self, job_id: &str) -> Option<ClusterAttemptId> {
        self.active.get(job_id).and_then(|a| a.cluster)
    }

    pub fn member_scores(&self, keys: &HashSet<&str>) -> HashMap<(String, String), WorkerJobScore> {
        self.scores
            .iter()
            .flat_map(|(worker, scores)| {
                scores
                    .iter()
                    .filter(|(key, _)| keys.contains(key.as_str()))
                    .map(|(key, score)| ((worker.clone(), key.clone()), score.clone()))
            })
            .collect()
    }

    pub fn remove_active(&mut self, job_id: &str) -> Option<PendingJob> {
        self.forget_job_scores(job_id);
        self.active.remove(job_id).map(|a| a.job)
    }

    fn forget_job_scores(&mut self, job_id: &str) {
        for ws in self.scores.values_mut() {
            ws.remove(job_id);
        }
    }

    pub fn active_job(&self, job_id: &str) -> Option<&PendingJob> {
        self.active.get(job_id).map(|a| &a.job)
    }

    pub fn active_eval_job(&self, job_id: &str) -> Option<&PendingEvalJob> {
        match self.active_job(job_id) {
            Some(PendingJob::Eval(j)) => Some(j),
            _ => None,
        }
    }

    pub fn active_build_job(&self, job_id: &str) -> Option<&PendingBuildJob> {
        match self.active_job(job_id) {
            Some(PendingJob::Build(j)) => Some(j),
            _ => None,
        }
    }

    pub fn pending_job(&self, job_id: &str) -> Option<&PendingJob> {
        self.pending.get(job_id)
    }

    pub fn drain_peer_jobs_on_worker(
        &mut self,
        worker_id: &str,
        revoked_peers: &HashSet<ProjectId>,
    ) -> Vec<String> {
        let to_requeue: Vec<String> = self
            .active
            .iter()
            .filter(|(_, a)| a.worker == worker_id && revoked_peers.contains(&a.job.project_id()))
            .map(|(id, _)| id.clone())
            .collect();
        for job_id in &to_requeue {
            if let Some(active) = self.active.remove(job_id)
                && active.cluster.is_none()
            {
                self.pending.insert(job_id.clone(), active.job);
            }
        }
        to_requeue
    }

    pub fn worker_disconnected(&mut self, worker_id: &str) -> Disconnected {
        self.scores.remove(worker_id);
        let orphaned: Vec<String> = self
            .active
            .iter()
            .filter(|(_, a)| a.worker == worker_id)
            .map(|(id, _)| id.clone())
            .collect();
        let mut gone = Disconnected::default();
        for job_id in orphaned {
            let Some(active) = self.active.remove(&job_id) else {
                continue;
            };
            match active.cluster {
                Some(attempt) => gone.cluster_members.push(LostMember {
                    attempt,
                    key: job_id,
                    job: active.job,
                }),
                None => {
                    gone.requeued.push(active.job.clone());
                    self.pending.insert(job_id, active.job);
                }
            }
        }
        gone
    }

    pub fn contains_job(&self, job_id: &str) -> bool {
        self.pending.contains_key(job_id)
            || self.active.contains_key(job_id)
            || self.clusters.contains(job_id)
    }

    pub fn remove_job(&mut self, job_id: &str) {
        self.forget_job_scores(job_id);
        self.pending.remove(job_id);
        self.active.remove(job_id);
        self.clusters.forget(job_id);
    }

    pub fn mark_aborting(&mut self, job_id: &str, now: Instant) {
        if let Some(active) = self.active.get_mut(job_id) {
            active.aborted_at.get_or_insert(now);
        }
    }

    pub fn take_overdue_aborts(&mut self, now: Instant, grace: Duration) -> Vec<(String, String)> {
        let overdue: Vec<String> = self
            .active
            .iter()
            .filter(|(_, a)| {
                a.aborted_at
                    .is_some_and(|at| now.duration_since(at) >= grace)
            })
            .map(|(id, _)| id.clone())
            .collect();

        overdue
            .into_iter()
            .filter_map(|job_id| {
                self.forget_job_scores(&job_id);
                let active = self.active.remove(&job_id)?;
                Some((active.worker, job_id))
            })
            .collect()
    }

    pub fn active_jobs(&self) -> impl Iterator<Item = (&str, &str, &PendingJob)> {
        self.active
            .iter()
            .map(|(job_id, a)| (job_id.as_str(), a.worker.as_str(), &a.job))
    }

    pub fn board_active_jobs(&self) -> Vec<BoardActiveJob> {
        self.active_jobs()
            .map(|(_, worker_id, job)| {
                let (architecture, required_features, fetch_step, eval_step) = match job {
                    PendingJob::Build(b) => (
                        Some(b.job.requirement.architecture.clone()),
                        b.job.requirement.required_features.clone(),
                        false,
                        false,
                    ),
                    PendingJob::Eval(e) => {
                        let fetch = e.job.steps.contains(&FlakeStep::FetchFlake);
                        let eval = e.job.steps.iter().any(|t| {
                            matches!(t, FlakeStep::EvaluateFlake | FlakeStep::EvaluateDerivations)
                        });
                        (None, Vec::new(), fetch, eval)
                    }
                };

                BoardActiveJob {
                    worker_id: worker_id.to_owned(),
                    project: job.project_id(),
                    kind: job.kind_disc(),
                    architecture,
                    required_features,
                    fetch_step,
                    eval_step,
                }
            })
            .collect()
    }

    pub fn remove_pending_for_evaluation(&mut self, evaluation_id: EvaluationId) {
        let removed: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, job)| job.evaluation_id() == evaluation_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &removed {
            self.pending.remove(id);
            self.forget_job_scores(id);
        }
        let members: Vec<String> = self
            .clusters
            .jobs()
            .filter(|(_, job)| job.evaluation_id() == evaluation_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &members {
            self.clusters.forget(key);
            self.forget_job_scores(key);
        }
    }

    pub fn prioritize(
        &mut self,
        evaluation: Option<EvaluationId>,
        shared_builds: &HashSet<DerivationBuildId>,
    ) {
        let active = self.active.values_mut().map(|a| &mut a.job);
        let members = self.clusters.jobs_mut();
        for job in self.pending.values_mut().chain(active).chain(members) {
            match job {
                PendingJob::Eval(e) if Some(e.evaluation_id) == evaluation => e.prioritized = true,
                PendingJob::Build(b) if shared_builds.contains(&b.derivation_build) => {
                    b.prioritized = true
                }
                _ => {}
            }
        }
    }

    pub fn prune_pending_builds(&mut self, stale: impl Fn(&PendingBuildJob) -> bool) -> usize {
        let pruned: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, job)| matches!(job, PendingJob::Build(b) if stale(b)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &pruned {
            self.pending.remove(id);
            self.forget_job_scores(id);
        }
        let members: Vec<String> = self
            .clusters
            .jobs()
            .filter(|(_, job)| matches!(job, PendingJob::Build(b) if stale(b)))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &members {
            self.clusters.forget(key);
            self.forget_job_scores(key);
        }

        pruned.len() + members.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn pending_snapshot(&self) -> Vec<PendingJobInfo> {
        self.pending
            .values()
            .map(|job| {
                let (kind, derivation_build, pname) = match job {
                    PendingJob::Build(b) => (
                        DispatchedJobKind::Build,
                        Some(b.derivation_build),
                        b.pname.clone(),
                    ),
                    PendingJob::Eval(_) => (DispatchedJobKind::Eval, None, None),
                };
                PendingJobInfo {
                    kind,
                    evaluation_id: job.evaluation_id(),
                    derivation_build,
                    project: job.project_id(),
                    queued_at: job.queued_at(),
                    dependency_count: job.dependency_count(),
                    pname,
                }
            })
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn instance_counts(&self) -> (u32, u32) {
        let active = self
            .active
            .values()
            .filter(|a| matches!(a.job, PendingJob::Build(_)))
            .count() as u32;
        let pending = self
            .pending
            .values()
            .filter(|j| matches!(j, PendingJob::Build(_)))
            .count() as u32;
        (active, pending)
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn bump_rescore_counts(&mut self) {
        for j in self.pending.values_mut() {
            j.set_rescore_count(j.rescore_count() + 1);
        }
    }

    #[cfg(test)]
    pub fn rescore_count_of(&self, job_id: &str) -> u32 {
        self.pending
            .get(job_id)
            .map(|j| j.rescore_count())
            .unwrap_or(0)
    }
}

#[cfg(test)]
pub(crate) fn test_eval_job(peer: ProjectId) -> PendingJob {
    tests::eval_job(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::{
        BuildJob, BuildRequirement, BuildSpec, BuildSpecKind, FlakeJob, FlakeSource, FlakeStep,
        GradientCapabilities,
    };

    #[test]
    fn a_pending_job_reports_its_own_key() {
        let peer = ProjectId::now_v7();

        let eval = eval_job(peer);
        let PendingJob::Eval(e) = &eval else {
            unreachable!("eval_job builds an eval")
        };
        assert_eq!(eval.job_key(), eval_job_key(e.evaluation_id));

        let build = build_job(peer, vec![]);
        let PendingJob::Build(b) = &build else {
            unreachable!("build_job builds a build")
        };
        assert_eq!(build.job_key(), build_job_key(b.derivation_build));
    }

    #[test]
    fn prioritize_lifts_the_evaluations_eval_job_and_the_named_builds_only() {
        let peer = ProjectId::now_v7();
        let eval = eval_job(peer);
        let other_eval = eval_job(peer);
        let named = build_job(peer, vec![]);
        let unnamed = build_job(peer, vec![]);
        let evaluation = eval.evaluation_id();
        let shared_build = named.derivation_build().expect("build");

        let mut tracker = JobTracker::new();
        for (key, job) in [
            ("e1", eval),
            ("e2", other_eval),
            ("b1", named),
            ("b2", unnamed),
        ] {
            tracker.add_pending(key.into(), job);
        }
        tracker.prioritize(Some(evaluation), &HashSet::from([shared_build]));

        let lifted = |key: &str| tracker.pending_job(key).expect("pending").prioritized();
        assert!(lifted("e1"));
        assert!(!lifted("e2"));
        assert!(lifted("b1"));
        assert!(!lifted("b2"));
    }

    #[test]
    fn only_a_pruned_evaluation_prunes_its_walk() {
        let peer = ProjectId::now_v7();
        let mut full = eval_job(peer);
        if let PendingJob::Eval(e) = &mut full {
            e.walk_mode = WalkMode::Full;
        }

        assert!(eval_job(peer).prunes_walk());
        assert!(!full.prunes_walk());
        assert!(!build_job(peer, vec![]).prunes_walk());
    }

    pub(super) fn eval_job(peer: ProjectId) -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: EvaluationId::now_v7(),
            task_id: None,
            project_id: peer,
            commit_id: CommitId::now_v7(),
            repository: "https://example.com/repo".into(),
            job: FlakeJob {
                steps: vec![FlakeStep::EvaluateDerivations],
                source: FlakeSource::Repository {
                    url: "https://example.com/repo".into(),
                    commit: "abc123".into(),
                },
                wildcards: vec!["*".into()],
                timeout_secs: None,
                input_overrides: vec![],
                input_update: None,
            },
            required_paths: vec![],
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            rescore_count: 0,
            prioritized: false,
            history: Default::default(),
            walk_mode: Default::default(),
        })
    }

    fn fetch_eval_job(peer: ProjectId) -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: EvaluationId::now_v7(),
            task_id: None,
            project_id: peer,
            commit_id: CommitId::now_v7(),
            repository: "git+ssh://git@example.com/repo".into(),
            job: FlakeJob {
                steps: vec![
                    FlakeStep::FetchFlake,
                    FlakeStep::EvaluateFlake,
                    FlakeStep::EvaluateDerivations,
                ],
                source: FlakeSource::Repository {
                    url: "git+ssh://git@example.com/repo".into(),
                    commit: "abc123".into(),
                },
                wildcards: vec!["*".into()],
                timeout_secs: None,
                input_overrides: vec![],
                input_update: None,
            },
            required_paths: vec![],
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            rescore_count: 0,
            prioritized: false,
            history: Default::default(),
            walk_mode: Default::default(),
        })
    }

    fn build_job(peer: ProjectId, required: Vec<RequiredPath>) -> PendingJob {
        build_job_arch(peer, required, "x86_64-linux", vec![])
    }

    fn record_for(tracker: &JobTracker, job_id: &str) -> AssignmentRecord {
        let sc = ScoredCandidate {
            total: 1.0,
            vetoed: false,
            score_breakdown: serde_json::json!({}),
            job_context: serde_json::json!({}),
        };

        assignment_record_for(
            job_id,
            tracker.pending_job(job_id).expect("the job is pending"),
            DispatchedJobId::now_v7(),
            &sc,
            serde_json::json!({}),
            serde_json::json!({}),
        )
    }

    fn build_job_arch(
        peer: ProjectId,
        required: Vec<RequiredPath>,
        architecture: &str,
        required_features: Vec<String>,
    ) -> PendingJob {
        let derivation_build = DerivationBuildId::now_v7();
        PendingJob::Build(PendingBuildJob {
            derivation_build,
            derivation: DerivationId::now_v7(),
            evaluation_id: EvaluationId::now_v7(),
            project_id: peer,
            job: BuildJob {
                builds: vec![BuildSpec {
                    build_id: derivation_build.to_string(),
                    drv_path: "/nix/store/abc.drv".into(),
                    kind: BuildSpecKind::Build,
                    is_fixed_output: false,
                    outputs: vec![],
                    timeout_secs: None,
                    max_silent_secs: None,
                }],
                requirement: BuildRequirement {
                    architecture: architecture.into(),
                    required_features,
                },
            },
            required_paths: required,
            dependency_count: 0,
            closure_size: None,
            prefer_local_build: false,
            is_fixed_output: false,
            history: gradient_pool::score::HistoryPrediction::default(),
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            rescore_count: 0,
            prioritized: false,
            pname: None,
            substitute: false,
        })
    }

    #[test]
    fn a_build_offer_carries_its_requirement_and_an_evaluation_offer_none() {
        let peer = ProjectId::now_v7();
        let build = build_job_arch(peer, vec![], "aarch64-linux", vec!["kvm".into()]);

        assert_eq!(
            build.as_candidate("build:1").requirement,
            Some(BuildRequirement {
                architecture: "aarch64-linux".into(),
                required_features: vec!["kvm".into()],
            })
        );
        assert_eq!(eval_job(peer).as_candidate("eval:1").requirement, None);
    }

    #[test]
    fn can_build_multi_arch_worker_accepts_one_of_many() {
        let caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into(), "aarch64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        assert!(caps.can_build("x86_64-linux", &[]));
        assert!(caps.can_build("aarch64-linux", &[]));
        assert!(!caps.can_build("riscv64-linux", &[]));
    }

    #[test]
    fn can_build_requires_all_features() {
        let caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec!["kvm".into()],
            ..Default::default()
        };
        assert!(caps.can_build("x86_64-linux", &["kvm".into()]));
        assert!(!caps.can_build("x86_64-linux", &["kvm".into(), "big-parallel".into()],));
    }

    fn flake_job(steps: Vec<FlakeStep>) -> FlakeJob {
        FlakeJob {
            steps,
            source: FlakeSource::Cached {
                store_path: "/nix/store/abc-source".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        }
    }

    #[test]
    fn can_eval_requires_eval_for_evaluation_steps() {
        let fetch_only = WorkerCaps {
            fetch: true,
            capabilities: GradientCapabilities {
                fetch: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let eval_only = WorkerCaps {
            fetch: false,
            capabilities: GradientCapabilities {
                eval: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let both = WorkerCaps {
            fetch: true,
            capabilities: GradientCapabilities {
                fetch: true,
                eval: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let bundled = flake_job(vec![
            FlakeStep::FetchFlake,
            FlakeStep::EvaluateFlake,
            FlakeStep::EvaluateDerivations,
        ]);
        let cached_eval = flake_job(vec![FlakeStep::EvaluateDerivations]);
        let fetch = flake_job(vec![FlakeStep::FetchFlake]);

        assert!(!fetch_only.can_eval(&bundled), "fetch-only lacks eval");
        assert!(!eval_only.can_eval(&bundled), "eval-only lacks fetch");
        assert!(both.can_eval(&bundled));
        assert!(eval_only.can_eval(&cached_eval));
        assert!(!fetch_only.can_eval(&cached_eval), "cached eval needs eval");
        assert!(fetch_only.can_eval(&fetch));
        assert!(!eval_only.can_eval(&fetch), "fetch step needs fetch");
    }

    #[test]
    fn add_pending_does_not_requeue_active_job() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("build:1".into(), build_job(peer, vec![]));
        let record = record_for(&tracker, "build:1");
        assert!(
            tracker
                .assign_pending("worker", "build:1", record)
                .is_some(),
            "job should assign"
        );
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);

        tracker.add_pending("build:1".into(), build_job(peer, vec![]));
        assert_eq!(
            tracker.pending_count(),
            0,
            "active job must not be re-queued"
        );
        assert_eq!(tracker.active_count(), 1);
    }

    fn assigned(tracker: &mut JobTracker, job_id: &str) {
        tracker.add_pending(job_id.into(), build_job(ProjectId::now_v7(), vec![]));
        let record = record_for(tracker, job_id);
        tracker
            .assign_pending("w1", job_id, record)
            .expect("the job assigns");
    }

    #[test]
    fn an_abort_the_worker_never_confirms_is_dropped_after_the_grace() {
        let mut tracker = JobTracker::new();
        assigned(&mut tracker, "build:1");
        assigned(&mut tracker, "build:2");
        let (t0, grace) = (Instant::now(), Duration::from_secs(300));

        tracker.mark_aborting("build:1", t0);

        assert!(
            tracker
                .take_overdue_aborts(t0 + grace - Duration::from_secs(1), grace)
                .is_empty(),
            "the worker still has time to confirm"
        );
        assert_eq!(
            tracker.take_overdue_aborts(t0 + grace, grace),
            vec![("w1".to_owned(), "build:1".to_owned())]
        );
        assert!(!tracker.contains_job("build:1"));
        assert!(
            tracker.contains_job("build:2"),
            "a job nobody aborted is never overdue"
        );
    }

    #[test]
    fn a_job_assigned_again_after_an_abort_starts_without_a_deadline() {
        let mut tracker = JobTracker::new();
        let (t0, grace) = (Instant::now(), Duration::from_secs(300));
        assigned(&mut tracker, "build:1");
        tracker.mark_aborting("build:1", t0);
        tracker.remove_active("build:1");

        assigned(&mut tracker, "build:1");

        assert!(tracker.take_overdue_aborts(t0 + grace, grace).is_empty());
        assert!(tracker.contains_job("build:1"));
    }

    #[test]
    fn a_repeated_abort_keeps_the_first_deadline() {
        let mut tracker = JobTracker::new();
        let (t0, grace) = (Instant::now(), Duration::from_secs(300));
        assigned(&mut tracker, "build:1");

        tracker.mark_aborting("build:1", t0);
        tracker.mark_aborting("build:1", t0 + grace);

        assert_eq!(tracker.take_overdue_aborts(t0 + grace, grace).len(), 1);
    }

    #[test]
    fn test_add_pending_and_candidates() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));
        tracker.add_pending("j3".into(), build_job(peer, vec![]));

        let candidates = tracker.candidates_for_worker(None, None);
        assert_eq!(candidates.len(), 3);
        assert_eq!(tracker.pending_count(), 3);
    }

    #[test]
    fn test_candidates_filtered_by_peer() {
        let mut tracker = JobTracker::new();
        let peer_a = ProjectId::now_v7();
        let peer_b = ProjectId::now_v7();
        tracker.add_pending("ja".into(), eval_job(peer_a));
        tracker.add_pending("jb".into(), eval_job(peer_b));

        let mut authorized = HashSet::new();
        authorized.insert(peer_a);

        let candidates = tracker.candidates_for_worker(Some(&authorized), None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].job_id, "ja");
    }

    #[test]
    fn test_candidates_filtered_by_architecture() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending(
            "x86".into(),
            build_job_arch(peer, vec![], "x86_64-linux", vec![]),
        );
        tracker.add_pending(
            "arm".into(),
            build_job_arch(peer, vec![], "aarch64-linux", vec![]),
        );
        tracker.add_pending(
            "any".into(),
            build_job_arch(peer, vec![], "builtin", vec![]),
        );

        let x86_caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let candidates = tracker.candidates_for_worker(None, Some(&x86_caps));
        let mut ids: Vec<_> = candidates.iter().map(|c| c.job_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["any".to_string(), "x86".to_string()]);
    }

    #[test]
    fn fetch_flake_job_requires_fetch_capability() {
        // Regression guard for #252. A FetchFlake job is cloning over SSH and must only go to
        // fetch-capable workers. Only those workers are receiving SSH credentials.
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), fetch_eval_job(peer));

        let no_fetch = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&no_fetch), &JobKind::Flake, &*p, &inst)
                .is_none(),
            "worker without fetch must not receive a fetch flake job"
        );
        assert_eq!(tracker.pending_count(), 1);

        let with_fetch = WorkerCaps {
            fetch: true,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            capabilities: GradientCapabilities {
                fetch: true,
                eval: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            tracker
                .take_best_of_kind("w2", None, Some(&with_fetch), &JobKind::Flake, &*p, &inst)
                .is_some(),
            "fetch- and eval-capable worker must receive the bundled flake job"
        );
    }

    #[test]
    fn cached_eval_job_requires_eval_not_fetch() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));

        let eval_no_fetch = WorkerCaps {
            fetch: false,
            architectures: vec![],
            system_features: vec![],
            capabilities: GradientCapabilities {
                eval: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind(
                    "w1",
                    None,
                    Some(&eval_no_fetch),
                    &JobKind::Flake,
                    &*p,
                    &inst
                )
                .is_some(),
            "cached eval job must run on an eval-capable worker without fetch"
        );
    }

    #[test]
    fn bundled_eval_job_skips_worker_without_eval() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), fetch_eval_job(peer));

        let fetch_build = WorkerCaps {
            fetch: true,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            capabilities: GradientCapabilities {
                fetch: true,
                build: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&fetch_build), &JobKind::Flake, &*p, &inst)
                .is_none(),
            "worker without eval must not receive a bundled eval job"
        );
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_take_best_of_kind_skips_wrong_arch() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending(
            "arm".into(),
            build_job_arch(peer, vec![], "aarch64-linux", vec![]),
        );
        let x86_caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        let assignment =
            tracker.take_best_of_kind("w1", None, Some(&x86_caps), &JobKind::Build, &*p, &inst);
        assert!(assignment.is_none());
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_take_best_of_kind_requires_features() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending(
            "kvm".into(),
            build_job_arch(peer, vec![], "x86_64-linux", vec!["kvm".into()]),
        );
        let no_kvm = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let with_kvm = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec!["kvm".into()],
            ..Default::default()
        };
        for w in ["w1", "w2"] {
            tracker.record_scores(
                w,
                vec![CandidateScore {
                    job_id: "kvm".into(),
                    missing_count: 0,
                    missing_nar_size: 0,
                    outputs_present: false,
                }],
            );
        }
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&no_kvm), &JobKind::Build, &*p, &inst)
                .is_none()
        );
        assert!(
            tracker
                .take_best_of_kind("w2", None, Some(&with_kvm), &JobKind::Build, &*p, &inst)
                .is_some()
        );
    }

    #[test]
    fn records_assign_decisions_including_rejected_candidates() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), build_job(peer, vec![]));
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();

        assert!(
            tracker
                .take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst)
                .is_none()
        );
        let decisions = tracker.recent_decisions();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].winner.is_none());
        assert_eq!(decisions[0].candidates.len(), 1);
        let candidate = &decisions[0].candidates[0];
        assert_eq!(candidate.job_id, "j1");
        assert!(!candidate.won);
        let vetoes = candidate.score_breakdown["vetoes"]
            .as_array()
            .expect("a vetoed candidate records its vetoes");
        assert!(
            vetoes.iter().any(|v| v == "RescoreWaitRule"),
            "the rescore hold must be visible on the rejected candidate"
        );

        tracker.record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        );
        assert!(
            tracker
                .take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst)
                .is_some()
        );
        let decisions = tracker.recent_decisions();
        assert_eq!(decisions.len(), 2, "most recent first");
        assert_eq!(decisions[0].winner.as_deref(), Some("j1"));
    }

    #[test]
    fn candidates_carry_ephemeral_id_and_breakdown_for_detail_lookup() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), build_job(peer, vec![]));
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();

        tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst);

        let decisions = tracker.recent_decisions();
        let cand = &decisions[0].candidates[0];
        assert!(
            !cand.won,
            "an uncached candidate is passed over, not dispatched"
        );
        assert!(
            cand.score_breakdown.get("rules").is_some(),
            "candidate must carry its per-rule breakdown for the detail page"
        );

        let detail = tracker
            .candidate_detail(cand.id)
            .expect("ephemeral id resolves to a candidate detail");
        assert_eq!(detail.id, cand.id);
        assert_eq!(detail.worker_id, "w1");
        assert!(!detail.won);
        assert!(detail.worker_context.is_object());

        assert!(
            tracker
                .candidate_detail(DispatchedJobId::now_v7())
                .is_none()
        );
    }

    #[test]
    fn terminal_job_removal_prunes_scores_across_all_workers() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();

        tracker.add_pending("build:j1".into(), build_job(peer, vec![]));
        for w in ["w1", "w2"] {
            tracker.record_scores(
                w,
                vec![CandidateScore {
                    job_id: "build:j1".into(),
                    missing_count: 0,
                    missing_nar_size: 0,
                    outputs_present: false,
                }],
            );
        }
        tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst);

        tracker.remove_active("build:j1");
        assert!(
            tracker
                .scores
                .values()
                .all(|ws| !ws.contains_key("build:j1")),
            "a completed job must not leak score entries"
        );

        tracker.add_pending("build:j2".into(), build_job(peer, vec![]));
        tracker.record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "build:j2".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        );
        tracker.remove_job("build:j2");
        assert!(
            tracker
                .scores
                .values()
                .all(|ws| !ws.contains_key("build:j2")),
            "an aborted job must not leak score entries"
        );
    }

    #[test]
    fn aborting_an_evaluation_prunes_its_pending_scores() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        let job = build_job(peer, vec![]);
        let eval_id = job.evaluation_id();

        tracker.add_pending("build:j1".into(), job);
        tracker.record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "build:j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        );
        tracker.remove_pending_for_evaluation(eval_id);
        assert!(
            tracker
                .scores
                .values()
                .all(|ws| !ws.contains_key("build:j1")),
            "aborting an evaluation must not leak its pending jobs' score entries"
        );
    }

    #[test]
    fn unscored_build_is_gated_until_scored() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending(
            "j1".into(),
            build_job(
                peer,
                vec![RequiredPath {
                    path: "/nix/store/foo".into(),
                    cache_info: None,
                }],
            ),
        );

        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();

        assert!(
            tracker
                .take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst)
                .is_none()
        );
        assert_eq!(tracker.pending_count(), 1);

        tracker.record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        );
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst);
        assert_eq!(assignment.unwrap().job_id(), "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[derive(Debug)]
    struct PreferAndVeto;

    impl gradient_pool::score::ScoreRule for PreferAndVeto {
        fn name(&self) -> &'static str {
            "PreferAndVeto"
        }
        fn score(
            &self,
            job: &JobContext<'_>,
            _: &WorkerContext<'_>,
            _: &gradient_pool::score::InstanceContext,
        ) -> f64 {
            if job.job.job_id == "vetoed" {
                10.0
            } else {
                1.0
            }
        }
        fn veto(
            &self,
            job: &JobContext<'_>,
            _: &WorkerContext<'_>,
            _: &gradient_pool::score::InstanceContext,
        ) -> bool {
            job.job.job_id == "vetoed"
        }
        fn description(&self) -> &'static str {
            "test"
        }
    }

    #[test]
    fn a_vetoed_top_candidate_yields_to_the_next_valid_one() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("vetoed".into(), build_job(peer, vec![]));
        tracker.add_pending("valid".into(), build_job(peer, vec![]));
        let p = gradient_pool::score::RulePolicy::new("test", vec![Box::new(PreferAndVeto)], false);
        let inst = gradient_pool::score::InstanceContext::default();

        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &p, &inst);

        assert_eq!(
            assignment.map(|a| a.job_id().to_owned()).as_deref(),
            Some("valid")
        );
        assert!(tracker.pending_job("vetoed").is_some());
        assert_eq!(
            tracker.recent_decisions()[0].winner.as_deref(),
            Some("valid")
        );
    }

    #[test]
    fn test_release_to_pending_after_rejection() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));

        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &*p, &inst);
        assert!(assignment.is_some());
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);

        tracker.release_to_pending("j1");
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.active_count(), 0);

        let candidates = tracker.candidates_for_worker(None, None);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_worker_disconnected_requeues() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));

        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        assert_eq!(tracker.active_count(), 2);
        assert_eq!(tracker.pending_count(), 0);

        let orphaned = tracker.worker_disconnected("w1").requeued;
        assert_eq!(orphaned.len(), 2);
        assert_eq!(tracker.pending_count(), 2);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_take_empty_required() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending(
            "j1".into(),
            build_job(
                peer,
                vec![RequiredPath {
                    path: "/nix/store/x".into(),
                    cache_info: None,
                }],
            ),
        );
        tracker.add_pending("j2".into(), eval_job(peer));

        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &*p, &inst);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id(), "j2");
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_aborts_only_revoked_project() {
        let mut tracker = JobTracker::new();
        let project_a = ProjectId::now_v7();
        let project_b = ProjectId::now_v7();
        tracker.add_pending("ja1".into(), eval_job(project_a));
        tracker.add_pending("ja2".into(), eval_job(project_a));
        tracker.add_pending("jb1".into(), eval_job(project_b));

        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        assert_eq!(tracker.active_jobs().count(), 3);

        let revoked = HashSet::from([project_a]);
        let aborted = tracker.drain_peer_jobs_on_worker("w1", &revoked);
        aborted.iter().for_each(|id| assert!(id.starts_with("ja")));
        assert_eq!(aborted.len(), 2);

        assert_eq!(tracker.active_jobs().count(), 1);
        assert_eq!(tracker.pending_count(), 2);
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_empty_revoked() {
        let mut tracker = JobTracker::new();
        let project_a = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(project_a));
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );

        let aborted = tracker.drain_peer_jobs_on_worker("w1", &HashSet::new());
        assert!(aborted.is_empty());
        assert_eq!(tracker.active_jobs().count(), 1);
    }

    #[test]
    fn test_contains_job_both_maps() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        assert!(tracker.contains_job("j1"));
        assert!(!tracker.contains_job("j2"));

        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        assert!(tracker.contains_job("j1"));
    }

    #[test]
    fn remove_job_drops_pending_entry() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        assert!(tracker.contains_job("j1"));
        tracker.remove_job("j1");
        assert!(!tracker.contains_job("j1"));
    }

    #[test]
    fn remove_job_drops_active_entry() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );
        assert!(tracker.contains_job("j1"));
        tracker.remove_job("j1");
        assert!(!tracker.contains_job("j1"));
    }

    #[test]
    fn pending_snapshot_reports_kind_and_project() {
        let mut tracker = JobTracker::new();
        let project = ProjectId::now_v7();
        tracker.add_pending("eval:1".into(), eval_job(project));
        let snap = tracker.pending_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].kind, DispatchedJobKind::Eval);
        assert_eq!(snap[0].project, project);
        assert!(snap[0].derivation_build.is_none());
    }

    #[test]
    fn cached_followup_rewrites_source_and_steps() {
        let peer = ProjectId::now_v7();
        let PendingJob::Eval(original) = fetch_eval_job(peer) else {
            unreachable!()
        };

        let follow = original.cached_followup("/nix/store/abc-source".into());

        assert_eq!(
            follow.job.steps,
            vec![FlakeStep::EvaluateFlake, FlakeStep::EvaluateDerivations]
        );
        match &follow.job.source {
            FlakeSource::Cached { store_path } => assert_eq!(store_path, "/nix/store/abc-source"),
            other => panic!("expected Cached, got {other:?}"),
        }
        assert_eq!(follow.evaluation_id, original.evaluation_id);
        assert_eq!(follow.project_id, original.project_id);
        assert_eq!(follow.repository, original.repository);
        assert_eq!(follow.required_paths.len(), 1);
        assert!(
            follow
                .required_paths
                .iter()
                .any(|p| p.path == "/nix/store/abc-source")
        );
    }

    #[test]
    fn bump_rescore_increments_pending_only() {
        let mut tracker = JobTracker::new();
        let peer = ProjectId::now_v7();
        tracker.add_pending("build:1".into(), build_job(peer, vec![]));

        tracker.bump_rescore_counts();
        tracker.bump_rescore_counts();
        assert_eq!(tracker.rescore_count_of("build:1"), 2);

        let record = record_for(&tracker, "build:1");
        tracker.assign_pending("worker", "build:1", record);
        tracker.bump_rescore_counts();
        assert_eq!(
            tracker.rescore_count_of("build:1"),
            0,
            "active job not bumped"
        );
    }

    #[test]
    fn is_fetch_only_true_only_for_fetch_step_alone() {
        let fetch_only = FlakeJob {
            steps: vec![FlakeStep::FetchFlake],
            source: FlakeSource::Repository {
                url: "u".into(),
                commit: "c".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        };
        assert!(is_fetch_only(&fetch_only));

        let bundled = FlakeJob {
            steps: vec![
                FlakeStep::FetchFlake,
                FlakeStep::EvaluateFlake,
                FlakeStep::EvaluateDerivations,
            ],
            ..fetch_only.clone()
        };
        assert!(!is_fetch_only(&bundled));

        let cached = FlakeJob {
            steps: vec![FlakeStep::EvaluateFlake, FlakeStep::EvaluateDerivations],
            ..fetch_only.clone()
        };
        assert!(!is_fetch_only(&cached));
    }

    fn member(cluster: ClusterJobId, count: u32) -> gradient_db::scheduling::cluster::MemberOf {
        crate::cluster::book::book_tests::member_of(cluster, count)
    }

    #[test]
    fn members_are_offered_for_scoring_but_never_assigned_singly() {
        let peer = ProjectId::now_v7();
        let mut tracker = JobTracker::new();
        tracker.add_member(
            member(ClusterJobId::now_v7(), 2),
            "m1".into(),
            eval_job(peer),
        );

        let offered = tracker.candidates_for_worker(None, None);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].job_id, "m1");
        assert!(tracker.contains_job("m1"));

        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind("w1", None, None, &JobKind::Flake, &*p, &inst)
                .is_none()
        );
    }

    #[test]
    fn a_pruned_member_unreadies_its_cluster() {
        let peer = ProjectId::now_v7();
        let cluster = ClusterJobId::now_v7();
        let mut tracker = JobTracker::new();
        tracker.add_member(
            member(cluster, 2),
            "build:a".into(),
            build_job(peer, vec![]),
        );
        tracker.add_member(member(cluster, 2), "eval:b".into(), eval_job(peer));
        assert_eq!(tracker.ready_clusters().count(), 1);

        assert_eq!(tracker.prune_pending_builds(|_| true), 1);

        assert_eq!(tracker.ready_clusters().count(), 0);
        assert!(!tracker.contains_job("build:a"));
        assert!(tracker.contains_job("eval:b"));
    }

    #[test]
    fn cancelling_an_evaluation_forgets_its_member() {
        let peer = ProjectId::now_v7();
        let job = eval_job(peer);
        let evaluation = job.evaluation_id();
        let mut tracker = JobTracker::new();
        tracker.add_member(member(ClusterJobId::now_v7(), 1), "eval:x".into(), job);

        tracker.remove_pending_for_evaluation(evaluation);

        assert!(!tracker.contains_job("eval:x"));
        assert_eq!(tracker.ready_clusters().count(), 0);
    }

    #[test]
    fn a_released_member_is_not_requeued() {
        let mut tracker = JobTracker::new();
        tracker.activate_members(
            ClusterAttemptId::now_v7(),
            vec![("w1".into(), "m1".into(), eval_job(ProjectId::now_v7()))],
        );

        tracker.release_to_pending("m1");

        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn a_disconnect_keeps_cluster_members_out_of_pending() {
        let peer = ProjectId::now_v7();
        let attempt = ClusterAttemptId::now_v7();
        let mut tracker = JobTracker::new();
        tracker.add_pending("single".into(), eval_job(peer));
        let p = gradient_pool::score::policy_by_name("simple");
        let inst = gradient_pool::score::InstanceContext::default();
        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &*p, &inst);
        tracker.activate_members(attempt, vec![("w1".into(), "m1".into(), eval_job(peer))]);
        assert_eq!(tracker.active_cluster("m1"), Some(attempt));

        let gone = tracker.worker_disconnected("w1");

        assert_eq!(gone.requeued.len(), 1);
        assert_eq!(gone.cluster_members.len(), 1);
        assert_eq!(
            (
                gone.cluster_members[0].attempt,
                gone.cluster_members[0].key.as_str()
            ),
            (attempt, "m1")
        );
        assert_eq!(tracker.pending_count(), 1);
        assert!(tracker.pending_job("m1").is_none());
    }

    #[test]
    fn a_member_record_is_scored_like_a_candidate() {
        let tracker = JobTracker::new();
        let shared_build = DerivationBuildId::now_v7();
        let job = PendingJob::Build(PendingBuildJob {
            substitute: true,
            ..crate::scheduler_tests::build_job(
                EvaluationId::now_v7(),
                ProjectId::now_v7(),
                shared_build,
            )
        });
        let key = build_job_key(shared_build);

        let rec = tracker.member_record(
            "w1",
            None,
            &key,
            &job,
            &*gradient_pool::score::policy_by_name("simple"),
            &gradient_pool::score::InstanceContext::default(),
        );

        assert_eq!(rec.job_id, key);
        assert_eq!(rec.derivation_build, Some(shared_build));
        assert!(rec.substitute);
        assert!(rec.score_breakdown.is_object(), "{}", rec.score_breakdown);
    }

    #[test]
    fn prioritizing_an_evaluation_lifts_its_waiting_cluster() {
        let job = eval_job(ProjectId::now_v7());
        let evaluation = job.evaluation_id();
        let mut tracker = JobTracker::new();
        tracker.add_member(member(ClusterJobId::now_v7(), 1), "eval:x".into(), job);

        tracker.prioritize(Some(evaluation), &HashSet::new());

        assert!(
            tracker
                .ready_clusters()
                .next()
                .expect("ready")
                .prioritized()
        );
    }

    #[test]
    fn a_member_activated_from_a_taken_cluster_leaves_the_book() {
        let cluster = ClusterJobId::now_v7();
        let job = eval_job(ProjectId::now_v7());
        let mut tracker = JobTracker::new();
        tracker.add_member(member(cluster, 1), "eval:x".into(), job.clone());
        tracker.take_cluster(cluster).expect("taken");
        assert!(tracker.contains_job("eval:x"), "tracked while claimed");

        tracker.activate_members(
            ClusterAttemptId::now_v7(),
            vec![("w1".into(), "eval:x".into(), job)],
        );
        tracker.remove_active("eval:x");

        assert!(!tracker.contains_job("eval:x"));
    }

    #[test]
    fn a_reattached_member_keeps_its_attempt() {
        let attempt = ClusterAttemptId::now_v7();
        let mut tracker = JobTracker::new();

        tracker.restore_active(
            "w1",
            Reattached {
                job_id: "m1".into(),
                job: eval_job(ProjectId::now_v7()),
                cluster: Some(attempt),
            },
        );
        tracker.release_to_pending("m1");

        assert_eq!(tracker.pending_count(), 0);
    }
}
