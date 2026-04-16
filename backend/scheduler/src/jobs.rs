/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pending and active job tracking.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use gradient_core::types::proto::{
    BuildJob, CandidateScore, FlakeJob, Job, JobCandidate, JobKind, RequiredPath,
};

#[derive(Debug, Clone)]
pub struct PendingEvalJob {
    pub evaluation_id: Uuid,
    pub project_id: Option<Uuid>,
    /// Peer (org/cache/proxy) that owns this job. Workers must be authorized
    /// for this peer to receive the job offer.
    pub peer_id: Uuid,
    pub commit_id: Uuid,
    pub repository: String,
    pub job: FlakeJob,
    pub required_paths: Vec<RequiredPath>,
}

#[derive(Debug, Clone)]
pub struct PendingBuildJob {
    pub build_id: Uuid,
    pub evaluation_id: Uuid,
    /// Peer (org/cache/proxy) that owns this job.
    pub peer_id: Uuid,
    pub job: BuildJob,
    pub required_paths: Vec<RequiredPath>,
}

#[derive(Debug, Clone)]
pub enum PendingJob {
    Eval(PendingEvalJob),
    Build(PendingBuildJob),
}

impl PendingJob {
    pub fn required_paths(&self) -> &[RequiredPath] {
        match self {
            PendingJob::Eval(j) => &j.required_paths,
            PendingJob::Build(j) => &j.required_paths,
        }
    }

    pub fn peer_id(&self) -> Uuid {
        match self {
            PendingJob::Eval(j) => j.peer_id,
            PendingJob::Build(j) => j.peer_id,
        }
    }

    pub fn as_candidate(&self, job_id: &str) -> JobCandidate {
        let drv_paths = match self {
            PendingJob::Build(j) => j.job.builds.iter().map(|t| t.drv_path.clone()).collect(),
            PendingJob::Eval(_) => vec![],
        };
        JobCandidate {
            job_id: job_id.to_owned(),
            required_paths: self.required_paths().to_vec(),
            drv_paths,
        }
    }

    fn into_job(self) -> Job {
        match self {
            PendingJob::Eval(j) => Job::Flake(j.job),
            PendingJob::Build(j) => Job::Build(j.job),
        }
    }

    pub fn evaluation_id(&self) -> Uuid {
        match self {
            PendingJob::Eval(j) => j.evaluation_id,
            PendingJob::Build(j) => j.evaluation_id,
        }
    }
}

pub struct Assignment {
    pub job_id: String,
    pub job: Job,
    /// Organization UUID that owns this job — used for credential lookup.
    pub peer_id: Uuid,
}

#[derive(Debug, Default)]
pub struct JobTracker {
    pending: HashMap<String, PendingJob>,
    scores: HashMap<String, HashMap<String, u32>>,
    active: HashMap<String, (String, PendingJob)>,
}

impl JobTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pending(&mut self, job_id: String, job: PendingJob) -> JobCandidate {
        let candidate = job.as_candidate(&job_id);
        self.pending.insert(job_id, job);
        candidate
    }

    /// Returns all pending job candidates that the worker is authorized to receive.
    /// Pass `None` to get all candidates (open/no-peer-restriction mode).
    pub fn candidates_for_worker(&self, authorized: Option<&HashSet<Uuid>>) -> Vec<JobCandidate> {
        self.pending
            .iter()
            .filter(|(_, job)| authorized.is_none_or(|peers| peers.contains(&job.peer_id())))
            .map(|(id, job)| job.as_candidate(id))
            .collect()
    }

    /// Process scores from a worker; assign the best-scoring candidate.
    /// Only considers jobs the worker is authorized for.
    ///
    /// The worker with the lowest `missing_nar_size` (fewest bytes to download)
    /// gets the job. `missing_count` is used as a tiebreaker when nar sizes are
    /// equal. Zero missing_nar_size and zero missing_count means the worker has
    /// all dependencies cached and can start immediately.
    pub fn receive_scores(
        &mut self,
        peer_id: &str,
        authorized: Option<&HashSet<Uuid>>,
        scores: Vec<CandidateScore>,
    ) -> Option<Assignment> {
        let worker_scores = self.scores.entry(peer_id.to_owned()).or_default();
        let mut best: Option<(String, u64, u32)> = None;

        for score in scores {
            let job = match self.pending.get(&score.job_id) {
                Some(j) => j,
                None => continue,
            };
            // Skip jobs this worker is not authorized for.
            if let Some(peers) = authorized
                && !peers.contains(&job.peer_id())
            {
                continue;
            }
            worker_scores.insert(score.job_id.clone(), score.missing_count);
            let is_better = match &best {
                None => true,
                Some((_, b_nar, b_cnt)) => {
                    score.missing_nar_size < *b_nar
                        || (score.missing_nar_size == *b_nar && score.missing_count < *b_cnt)
                }
            };
            if is_better {
                best = Some((score.job_id, score.missing_nar_size, score.missing_count));
            }
        }

        let (job_id, _, _) = best?;
        self.assign_pending(peer_id, &job_id)
    }

    /// Assign the first pending job matching `kind`, restricted to authorized peers.
    /// Used for pull-based `RequestJob` dispatch (no scoring needed).
    pub fn take_first_of_kind(
        &mut self,
        peer_id: &str,
        authorized: Option<&HashSet<Uuid>>,
        kind: &JobKind,
    ) -> Option<Assignment> {
        let job_id = self
            .pending
            .iter()
            .filter(|(_, j)| {
                authorized.is_none_or(|peers| peers.contains(&j.peer_id()))
                    && matches!(
                        (kind, j),
                        (JobKind::Flake, PendingJob::Eval(_)) | (JobKind::Build, PendingJob::Build(_))
                    )
            })
            .map(|(id, _)| id.clone())
            .next()?;
        self.assign_pending(peer_id, &job_id)
    }

    /// Assign any pending job with no required paths, restricted to authorized peers.
    pub fn take_empty_required(
        &mut self,
        peer_id: &str,
        authorized: Option<&HashSet<Uuid>>,
    ) -> Option<Assignment> {
        let job_id = self
            .pending
            .iter()
            .filter(|(_, j)| {
                j.required_paths().is_empty()
                    && authorized.is_none_or(|peers| peers.contains(&j.peer_id()))
            })
            .map(|(id, _)| id.clone())
            .next()?;
        self.assign_pending(peer_id, &job_id)
    }

    fn assign_pending(&mut self, peer_id: &str, job_id: &str) -> Option<Assignment> {
        let job = self.pending.remove(job_id)?;
        if let Some(ws) = self.scores.get_mut(peer_id) {
            ws.remove(job_id);
        }
        let assignment = Assignment {
            job_id: job_id.to_owned(),
            job: job.clone().into_job(),
            peer_id: job.peer_id(),
        };
        self.active
            .insert(job_id.to_owned(), (peer_id.to_owned(), job));
        Some(assignment)
    }

    pub fn release_to_pending(&mut self, job_id: &str) {
        if let Some((_, job)) = self.active.remove(job_id) {
            self.pending.insert(job_id.to_owned(), job);
        }
    }

    pub fn remove_active(&mut self, job_id: &str) -> Option<PendingJob> {
        self.active.remove(job_id).map(|(_, j)| j)
    }

    pub fn active_job(&self, job_id: &str) -> Option<&PendingJob> {
        self.active.get(job_id).map(|(_, j)| j)
    }

    /// Move all active jobs assigned to `worker_id` that belong to any of
    /// `revoked_peers` back to the pending queue.  Returns the job IDs so the
    /// caller can send `AbortJob` messages to the worker.
    pub fn drain_peer_jobs_on_worker(
        &mut self,
        worker_id: &str,
        revoked_peers: &HashSet<Uuid>,
    ) -> Vec<String> {
        let to_requeue: Vec<String> = self
            .active
            .iter()
            .filter(|(_, (w, job))| w == worker_id && revoked_peers.contains(&job.peer_id()))
            .map(|(id, _)| id.clone())
            .collect();
        for job_id in &to_requeue {
            if let Some((_, job)) = self.active.remove(job_id) {
                self.pending.insert(job_id.clone(), job);
            }
        }
        to_requeue
    }

    pub fn worker_disconnected(&mut self, peer_id: &str) -> Vec<String> {
        self.scores.remove(peer_id);
        let orphaned: Vec<String> = self
            .active
            .iter()
            .filter(|(_, (w, _))| w == peer_id)
            .map(|(id, _)| id.clone())
            .collect();
        for job_id in &orphaned {
            if let Some((_, job)) = self.active.remove(job_id) {
                self.pending.insert(job_id.clone(), job);
            }
        }
        orphaned
    }

    pub fn contains_job(&self, job_id: &str) -> bool {
        self.pending.contains_key(job_id) || self.active.contains_key(job_id)
    }

    /// Iterate over active jobs: yields `(job_id, worker_id, &PendingJob)`.
    pub fn active_jobs(&self) -> impl Iterator<Item = (&str, &str, &PendingJob)> {
        self.active
            .iter()
            .map(|(job_id, (worker_id, job))| (job_id.as_str(), worker_id.as_str(), job))
    }

    /// Remove all pending (unassigned) jobs belonging to a given evaluation.
    pub fn remove_pending_for_evaluation(&mut self, evaluation_id: Uuid) {
        self.pending
            .retain(|_, job| job.evaluation_id() != evaluation_id);
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::types::proto::{BuildJob, BuildTask, FlakeJob, FlakeTask};

    fn eval_job(peer: Uuid) -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: Uuid::new_v4(),
            project_id: None,
            peer_id: peer,
            commit_id: Uuid::new_v4(),
            repository: "https://example.com/repo".into(),
            job: FlakeJob {
                tasks: vec![FlakeTask::EvaluateDerivations],
                repository: "https://example.com/repo".into(),
                commit: "abc123".into(),
                wildcards: vec!["*".into()],
                timeout_secs: None,
            sign: None,
            },
            required_paths: vec![],
        })
    }

    fn build_job(peer: Uuid, required: Vec<RequiredPath>) -> PendingJob {
        PendingJob::Build(PendingBuildJob {
            build_id: Uuid::new_v4(),
            evaluation_id: Uuid::new_v4(),
            peer_id: peer,
            job: BuildJob {
                builds: vec![BuildTask {
                    build_id: Uuid::new_v4().to_string(),
                    drv_path: "/nix/store/abc.drv".into(),
                }],
                compress: None,
                sign: None,
            },
            required_paths: required,
        })
    }

    #[test]
    fn test_add_pending_and_candidates() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));
        tracker.add_pending("j3".into(), build_job(peer, vec![]));

        let candidates = tracker.candidates_for_worker(None);
        assert_eq!(candidates.len(), 3);
        assert_eq!(tracker.pending_count(), 3);
    }

    #[test]
    fn test_candidates_filtered_by_peer() {
        let mut tracker = JobTracker::new();
        let peer_a = Uuid::new_v4();
        let peer_b = Uuid::new_v4();
        tracker.add_pending("ja".into(), eval_job(peer_a));
        tracker.add_pending("jb".into(), eval_job(peer_b));

        let mut authorized = HashSet::new();
        authorized.insert(peer_a);

        let candidates = tracker.candidates_for_worker(Some(&authorized));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].job_id, "ja");
    }

    #[test]
    fn test_receive_scores_assigns_zero_missing() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
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

        let assignment = tracker.receive_scores(
            "w1",
            None,
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
            }],
        );
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_receive_scores_assigns_nonzero_best() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
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

        // Even with missing > 0, the best candidate is assigned.
        let assignment = tracker.receive_scores(
            "w1",
            None,
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 5,
                missing_nar_size: 0,
            }],
        );
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_release_to_pending_after_rejection() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));

        // Assign it.
        let assignment = tracker.take_empty_required("w1", None);
        assert!(assignment.is_some());
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);

        // Release it back.
        tracker.release_to_pending("j1");
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.active_count(), 0);

        // Should reappear in candidates.
        let candidates = tracker.candidates_for_worker(None);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_worker_disconnected_requeues() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));

        tracker.take_empty_required("w1", None);
        tracker.take_empty_required("w1", None);
        assert_eq!(tracker.active_count(), 2);
        assert_eq!(tracker.pending_count(), 0);

        let orphaned = tracker.worker_disconnected("w1");
        assert_eq!(orphaned.len(), 2);
        assert_eq!(tracker.pending_count(), 2);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_take_empty_required() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        // Job with required paths — should NOT be taken.
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
        // Job with no required paths — should be taken.
        tracker.add_pending("j2".into(), eval_job(peer));

        let assignment = tracker.take_empty_required("w1", None);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j2");
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_aborts_only_revoked_org() {
        let mut tracker = JobTracker::new();
        let org_a = Uuid::new_v4();
        let org_b = Uuid::new_v4();
        tracker.add_pending("ja1".into(), eval_job(org_a));
        tracker.add_pending("ja2".into(), eval_job(org_a));
        tracker.add_pending("jb1".into(), eval_job(org_b));

        // Assign all three to worker w1.
        tracker.take_empty_required("w1", None);
        tracker.take_empty_required("w1", None);
        tracker.take_empty_required("w1", None);
        assert_eq!(tracker.active_jobs().count(), 3);

        // Revoke only org_a.
        let revoked = HashSet::from([org_a]);
        let aborted = tracker.drain_peer_jobs_on_worker("w1", &revoked);
        aborted.iter().for_each(|id| assert!(id.starts_with("ja")));
        assert_eq!(aborted.len(), 2);

        // org_b job is still active; org_a jobs are back in pending.
        assert_eq!(tracker.active_jobs().count(), 1);
        assert_eq!(tracker.pending_count(), 2);
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_empty_revoked() {
        let mut tracker = JobTracker::new();
        let org_a = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(org_a));
        tracker.take_empty_required("w1", None);

        let aborted = tracker.drain_peer_jobs_on_worker("w1", &HashSet::new());
        assert!(aborted.is_empty());
        assert_eq!(tracker.active_jobs().count(), 1);
    }

    #[test]
    fn test_contains_job_both_maps() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));
        assert!(tracker.contains_job("j1"));
        assert!(!tracker.contains_job("j2"));

        tracker.take_empty_required("w1", None);
        // Now in active, not pending — should still be "contained".
        assert!(tracker.contains_job("j1"));
    }
}
