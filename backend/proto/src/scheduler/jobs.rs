/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pending and active job tracking.

use std::collections::HashMap;

use uuid::Uuid;

use crate::messages::{FlakeJob, BuildJob, Job, JobCandidate, CandidateScore};

#[derive(Debug, Clone)]
pub struct PendingEvalJob {
    pub evaluation_id: Uuid,
    pub project_id: Option<Uuid>,
    pub organization_id: Uuid,
    pub commit_id: Uuid,
    pub repository: String,
    pub job: FlakeJob,
    pub required_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PendingBuildJob {
    pub build_id: Uuid,
    pub evaluation_id: Uuid,
    pub job: BuildJob,
    pub required_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum PendingJob {
    Eval(PendingEvalJob),
    Build(PendingBuildJob),
}

impl PendingJob {
    pub fn required_paths(&self) -> &[String] {
        match self {
            PendingJob::Eval(j) => &j.required_paths,
            PendingJob::Build(j) => &j.required_paths,
        }
    }

    pub fn as_candidate(&self, job_id: &str) -> JobCandidate {
        JobCandidate {
            job_id: job_id.to_owned(),
            required_paths: self.required_paths().to_vec(),
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

    pub fn all_candidates(&self) -> Vec<JobCandidate> {
        self.pending
            .iter()
            .map(|(id, job)| job.as_candidate(id))
            .collect()
    }

    /// Process scores from a worker; assign if best score is 0.
    pub fn receive_scores(
        &mut self,
        peer_id: &str,
        scores: Vec<CandidateScore>,
    ) -> Option<Assignment> {
        let worker_scores = self.scores.entry(peer_id.to_owned()).or_default();
        let mut best: Option<(String, u32)> = None;

        for score in scores {
            if !self.pending.contains_key(&score.job_id) {
                continue;
            }
            worker_scores.insert(score.job_id.clone(), score.missing);
            match &best {
                None => best = Some((score.job_id, score.missing)),
                Some((_, b)) if score.missing < *b => {
                    best = Some((score.job_id, score.missing));
                }
                _ => {}
            }
        }

        let (job_id, missing) = best?;
        if missing != 0 {
            return None;
        }

        self.assign_pending(peer_id, &job_id)
    }

    /// Assign any pending job that has no required paths (will assign immediately).
    pub fn take_empty_required(&mut self, peer_id: &str) -> Option<Assignment> {
        let job_id = self
            .pending
            .iter()
            .find(|(_, j)| j.required_paths().is_empty())
            .map(|(id, _)| id.clone())?;
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
        };
        self.active.insert(job_id.to_owned(), (peer_id.to_owned(), job));
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

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}
