/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Composable scoring policy for job-to-worker assignment.
//!
//! When a worker sends `RequestJob`, the scheduler ranks every eligible pending
//! job by summing the scores returned by each [`Rule`] in the active [`Policy`].
//! The highest-scoring job is assigned.
//!
//! # Adding a rule
//!
//! ```rust,ignore
//! let mut policy = Policy::default();
//! policy.add_rule(MyRule { weight: 5.0 });
//! ```
//!
//! Rules run in insertion order but the final decision is the sum — order only
//! matters when a tie needs to be broken deterministically.

use crate::jobs::PendingJob;

// ── Context types ─────────────────────────────────────────────────────────────

/// Everything the policy knows about the candidate job at scoring time.
#[derive(Debug, Clone, Copy)]
pub struct JobContext<'a> {
    pub job: &'a PendingJob,
    /// Number of required store paths the worker reported as missing.
    /// `None` when the worker has not yet submitted a score for this job.
    pub missing_count: Option<u32>,
    /// Total uncompressed NAR size (bytes) of the missing paths.
    /// `None` when no score has been submitted.
    pub missing_nar_size: Option<u64>,
    /// Number of direct derivation dependencies this build has.
    pub dependency_count: u32,
    /// When the build entered the queue (`build.updated_at`).
    /// Used to prefer builds that have waited longer.
    pub queued_at: chrono::NaiveDateTime,
}

/// Build-relevant capabilities of the requesting worker.
#[derive(Debug, Clone, Copy)]
pub struct WorkerContext<'a> {
    pub architectures: &'a [String],
    pub system_features: &'a [String],
}

// ── Rule trait ────────────────────────────────────────────────────────────────

/// A single scoring rule.
///
/// Returns a `f64` contribution to the total job score.  Positive values
/// favour assignment; negative values disfavour it.  Rules can return values
/// of any magnitude — there is no required scale, but the built-in rules use
/// the following rough ranges so custom rules can be written consistently:
///
/// | Range | Meaning |
/// |-------|---------|
/// | ≥ 500 | Hard preference — nearly always wins |
/// | 100–500 | Strong preference |
/// | 0–100 | Soft preference |
/// | −100–0 | Soft disfavour |
/// | ≤ −500 | Hard penalty — nearly always loses |
pub trait Rule: Send + Sync + std::fmt::Debug {
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64;
}

// ── Built-in rules ────────────────────────────────────────────────────────────

/// Prefer jobs where the worker already has most required store paths.
///
/// When a worker has submitted a score for a job:
/// - 0 missing paths → `+scored_bonus` (full bonus)
/// - Each missing path → `−path_penalty`
///
/// When no score has been submitted (worker hasn't checked yet), the job
/// gets `0.0` — scored candidates always win over unscored ones as long as
/// `scored_bonus > 0`.
#[derive(Debug)]
pub struct MissingPathsRule {
    /// Flat bonus awarded when the worker has submitted any score (≥ 0).
    pub scored_bonus: f64,
    /// Penalty per missing store path (should be positive — it is subtracted).
    pub path_penalty: f64,
}

impl Default for MissingPathsRule {
    fn default() -> Self {
        Self {
            scored_bonus: 200.0,
            path_penalty: 10.0,
        }
    }
}

impl Rule for MissingPathsRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        match job.missing_count {
            None => 0.0,
            Some(n) => self.scored_bonus - (n as f64) * self.path_penalty,
        }
    }
}

/// Penalise jobs that require the worker to fetch large amounts of NAR data.
///
/// Score contribution: `−(missing_nar_size_mb * size_penalty_per_mb)`.
/// Returns `0.0` when no size information is available.
#[derive(Debug)]
pub struct MissingNarSizeRule {
    /// Penalty per megabyte of missing NAR data (should be positive).
    pub size_penalty_per_mb: f64,
}

impl Default for MissingNarSizeRule {
    fn default() -> Self {
        Self {
            size_penalty_per_mb: 1.0,
        }
    }
}

impl Rule for MissingNarSizeRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        match job.missing_nar_size {
            None => 0.0,
            Some(bytes) => {
                let mb = bytes as f64 / 1_048_576.0;
                -(mb * self.size_penalty_per_mb)
            }
        }
    }
}

/// Slightly deprioritise `builtin:*` derivations (e.g. `builtin:fetchurl`).
///
/// These are synthetic helper builds that run on any worker.  Giving them a
/// small penalty lets real architecture-specific builds go first, so dedicated
/// workers are not occupied with fetch-only work when actual compilation is
/// waiting.
#[derive(Debug)]
pub struct BuiltinDeprioritizeRule {
    /// Penalty applied to jobs with `architecture == "builtin"` (positive = penalty).
    pub penalty: f64,
}

impl Default for BuiltinDeprioritizeRule {
    fn default() -> Self {
        Self { penalty: 50.0 }
    }
}

impl Rule for BuiltinDeprioritizeRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        match job.job {
            PendingJob::Build(j) if j.architecture == "builtin" => -self.penalty,
            _ => 0.0,
        }
    }
}

/// Prefer builds that have more derivation dependencies.
///
/// A build with many inputs is likely a "root" that unblocks a large portion
/// of the dependency graph once it finishes, so scheduling it early reduces
/// overall wall-clock time for the evaluation.
///
/// Score contribution: `dependency_count as f64 * dep_bonus_per_dep`.
#[derive(Debug)]
pub struct DependencyCountRule {
    /// Score bonus per direct dependency (should be positive).
    pub dep_bonus_per_dep: f64,
}

impl Default for DependencyCountRule {
    fn default() -> Self {
        Self {
            dep_bonus_per_dep: 0.5,
        }
    }
}

impl Rule for DependencyCountRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        match job.job {
            PendingJob::Build(j) => j.dependency_count as f64 * self.dep_bonus_per_dep,
            PendingJob::Eval(_) => 0.0,
        }
    }
}

/// Prefer builds that have been waiting in the queue the longest.
///
/// Score contribution: seconds since `queued_at`, capped at `max_wait_secs`,
/// scaled by `bonus_per_second`.
#[derive(Debug)]
pub struct WaitTimeRule {
    /// Score bonus per second the build has been waiting (should be positive).
    pub bonus_per_second: f64,
    /// Cap on wait time used for scoring, in seconds.
    /// Prevents very old jobs from dominating all other rules.
    pub max_wait_secs: f64,
}

impl Default for WaitTimeRule {
    fn default() -> Self {
        Self {
            bonus_per_second: 0.1,
            max_wait_secs: 600.0, // 10 minutes
        }
    }
}

impl Rule for WaitTimeRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        let now = chrono::Utc::now().naive_utc();
        let waited = (now - job.queued_at)
            .num_seconds()
            .max(0) as f64;
        waited.min(self.max_wait_secs) * self.bonus_per_second
    }
}

// ── Policy ────────────────────────────────────────────────────────────────────

/// Ordered collection of [`Rule`]s.
///
/// Scores a (job, worker) pair by summing every rule's contribution.
/// The scheduler picks the job with the **highest** total score.
#[derive(Debug, Default)]
pub struct Policy {
    rules: Vec<Box<dyn Rule>>,
}

impl Policy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_rule<R: Rule + 'static>(&mut self, rule: R) {
        self.rules.push(Box::new(rule));
    }

    /// Compute the total score for assigning `job` to the requesting `worker`.
    ///
    /// Higher is better.  Returns `0.0` when the policy has no rules.
    pub fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        self.rules.iter().map(|r| r.score(job, worker)).sum()
    }

    /// Construct the default scheduling policy used by the server.
    ///
    /// Rules (in evaluation order):
    /// 1. [`MissingPathsRule`] — prefer jobs the worker can start without fetching
    /// 2. [`MissingNarSizeRule`] — prefer jobs that require less data to fetch
    /// 3. [`DependencyCountRule`] — prefer builds that unblock more downstream work
    /// 4. [`WaitTimeRule`] — prevent starvation by boosting long-waiting builds
    /// 5. [`BuiltinDeprioritizeRule`] — keep real builds ahead of synthetic helpers
    pub fn default_build_policy() -> Self {
        let mut p = Self::new();
        p.add_rule(MissingPathsRule::default());
        p.add_rule(MissingNarSizeRule::default());
        p.add_rule(DependencyCountRule::default());
        p.add_rule(WaitTimeRule::default());
        p.add_rule(BuiltinDeprioritizeRule::default());
        p
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{PendingBuildJob, PendingEvalJob, PendingJob};
    use gradient_core::types::proto::{BuildJob, BuildTask, FlakeJob, FlakeTask};
    use uuid::Uuid;

    fn worker_ctx<'a>(archs: &'a [String], features: &'a [String]) -> WorkerContext<'a> {
        WorkerContext {
            architectures: archs,
            system_features: features,
        }
    }

    fn build_job_ctx(arch: &str, missing_count: Option<u32>, missing_nar_size: Option<u64>) -> (PendingJob, u32, u64) {
        let job = PendingJob::Build(PendingBuildJob {
            build_id: Uuid::new_v4(),
            evaluation_id: Uuid::new_v4(),
            peer_id: Uuid::new_v4(),
            job: BuildJob {
                builds: vec![BuildTask {
                    build_id: Uuid::new_v4().to_string(),
                    drv_path: "/nix/store/abc.drv".into(),
                }],
                compress: None,
                sign: None,
            },
            required_paths: vec![],
            architecture: arch.into(),
            required_features: vec![],
            dependency_count: 0,
            queued_at: chrono::Utc::now().naive_utc(),
        });
        (job, missing_count.unwrap_or(0), missing_nar_size.unwrap_or(0))
    }

    #[test]
    fn missing_paths_rule_scored_zero_wins() {
        let rule = MissingPathsRule::default();
        let archs = vec!["x86_64-linux".into()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);

        let now = chrono::Utc::now().naive_utc();
        let (job, ..) = build_job_ctx("x86_64-linux", Some(0), None);
        let ctx_scored = JobContext { job: &job, missing_count: Some(0), missing_nar_size: None, dependency_count: 0, queued_at: now };
        let (job2, ..) = build_job_ctx("x86_64-linux", None, None);
        let ctx_unscored = JobContext { job: &job2, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };

        // Scored with 0 missing beats unscored.
        assert!(rule.score(&ctx_scored, &w) > rule.score(&ctx_unscored, &w));
    }

    #[test]
    fn missing_paths_rule_fewer_missing_wins() {
        let rule = MissingPathsRule::default();
        let archs = vec!["x86_64-linux".into()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);

        let now = chrono::Utc::now().naive_utc();
        let (j1, ..) = build_job_ctx("x86_64-linux", Some(2), None);
        let (j2, ..) = build_job_ctx("x86_64-linux", Some(10), None);
        let c1 = JobContext { job: &j1, missing_count: Some(2), missing_nar_size: None, dependency_count: 0, queued_at: now };
        let c2 = JobContext { job: &j2, missing_count: Some(10), missing_nar_size: None, dependency_count: 0, queued_at: now };

        assert!(rule.score(&c1, &w) > rule.score(&c2, &w));
    }

    #[test]
    fn missing_nar_size_rule_smaller_wins() {
        let rule = MissingNarSizeRule::default();
        let archs = vec!["x86_64-linux".into()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);

        let now = chrono::Utc::now().naive_utc();
        let (j1, ..) = build_job_ctx("x86_64-linux", None, None);
        let (j2, ..) = build_job_ctx("x86_64-linux", None, None);
        let c1 = JobContext { job: &j1, missing_count: None, missing_nar_size: Some(1_048_576), dependency_count: 0, queued_at: now }; // 1 MB
        let c2 = JobContext { job: &j2, missing_count: None, missing_nar_size: Some(100_000_000), dependency_count: 0, queued_at: now }; // ~95 MB

        assert!(rule.score(&c1, &w) > rule.score(&c2, &w));
    }

    #[test]
    fn builtin_deprioritize_rule_penalises_builtin() {
        let rule = BuiltinDeprioritizeRule::default();
        let archs = vec!["x86_64-linux".into()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);

        let now = chrono::Utc::now().naive_utc();
        let (j_real, ..) = build_job_ctx("x86_64-linux", None, None);
        let (j_builtin, ..) = build_job_ctx("builtin", None, None);
        let c_real = JobContext { job: &j_real, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };
        let c_builtin = JobContext { job: &j_builtin, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };

        assert!(rule.score(&c_real, &w) > rule.score(&c_builtin, &w));
    }

    #[test]
    fn default_policy_prefers_ready_over_costly() {
        let policy = Policy::default_build_policy();
        let archs = vec!["x86_64-linux".into()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);

        let now = chrono::Utc::now().naive_utc();
        // Job A: worker has everything, real arch
        let (ja, ..) = build_job_ctx("x86_64-linux", Some(0), Some(0));
        let ca = JobContext { job: &ja, missing_count: Some(0), missing_nar_size: Some(0), dependency_count: 0, queued_at: now };

        // Job B: worker missing 5 paths, large NAR, builtin
        let (jb, ..) = build_job_ctx("builtin", Some(5), Some(50_000_000));
        let cb = JobContext { job: &jb, missing_count: Some(5), missing_nar_size: Some(50_000_000), dependency_count: 0, queued_at: now };

        assert!(policy.score(&ca, &w) > policy.score(&cb, &w));
    }
}
