/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::JobKindView;
use crate::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug)]
pub struct MissingPathsRule {
    pub scored_bonus: f64,
    pub path_penalty: f64,
}

impl Default for MissingPathsRule {
    fn default() -> Self {
        Self { scored_bonus: 200.0, path_penalty: 10.0 }
    }
}

impl ScoreRule for MissingPathsRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        match job.missing_count {
            None => 0.0,
            Some(n) => self.scored_bonus - (n as f64) * self.path_penalty,
        }
    }
}

#[derive(Debug)]
pub struct MissingNarSizeRule {
    pub size_penalty_per_mb: f64,
}

impl Default for MissingNarSizeRule {
    fn default() -> Self {
        Self { size_penalty_per_mb: 1.0 }
    }
}

impl ScoreRule for MissingNarSizeRule {
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

#[derive(Debug)]
pub struct BuiltinDeprioritizeRule {
    pub penalty: f64,
}

impl Default for BuiltinDeprioritizeRule {
    fn default() -> Self {
        Self { penalty: 50.0 }
    }
}

impl ScoreRule for BuiltinDeprioritizeRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        if job.job.kind == JobKindView::Build && job.job.architecture == "builtin" {
            -self.penalty
        } else {
            0.0
        }
    }
}

#[derive(Debug)]
pub struct DependencyCountRule {
    pub dep_bonus_per_dep: f64,
}

impl Default for DependencyCountRule {
    fn default() -> Self {
        Self { dep_bonus_per_dep: 0.5 }
    }
}

impl ScoreRule for DependencyCountRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        if job.job.kind == JobKindView::Build {
            job.dependency_count as f64 * self.dep_bonus_per_dep
        } else {
            0.0
        }
    }
}

#[derive(Debug)]
pub struct WaitTimeRule {
    pub bonus_per_second: f64,
    pub max_wait_secs: f64,
}

impl Default for WaitTimeRule {
    fn default() -> Self {
        Self { bonus_per_second: 0.1, max_wait_secs: 3600.0 }
    }
}

impl ScoreRule for WaitTimeRule {
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        let now = gradient_core::types::now();
        let waited = (now - job.queued_at).num_seconds().max(0) as f64;
        waited.min(self.max_wait_secs) * self.bonus_per_second
    }
}

#[derive(Debug)]
pub struct ReserveFetchWorkersRule {
    pub penalty: f64,
}

impl Default for ReserveFetchWorkersRule {
    fn default() -> Self {
        Self { penalty: 300.0 }
    }
}

impl ScoreRule for ReserveFetchWorkersRule {
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        match job.job.kind {
            JobKindView::Eval { fetch_flake } if worker.fetch && !fetch_flake => -self.penalty,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob};
    use gradient_core::types::ids::OrganizationId;

    fn test_job(kind: JobKindView, arch: &'static str) -> ScoredJob<'static> {
        ScoredJob::new(
            "test",
            OrganizationId::now_v7(),
            kind,
            arch,
            false,
            LazyProviders {
                closure_size: &|| None,
                history: &|| HistoryPrediction::default(),
            },
        )
    }

    fn worker<'a>(archs: &'a [String], fetch: bool) -> WorkerContext<'a> {
        WorkerContext { architectures: archs, system_features: &[], fetch, metrics: None }
    }

    #[test]
    fn missing_paths_scored_zero_wins_over_unscored() {
        let rule = MissingPathsRule::default();
        let job = test_job(JobKindView::Build, "x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let scored = JobContext { job: &job, missing_count: Some(0), missing_nar_size: None, dependency_count: 0, queued_at: now };
        let unscored = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };

        assert!(rule.score(&scored, &w) > rule.score(&unscored, &w));
    }

    #[test]
    fn missing_paths_fewer_missing_wins() {
        let rule = MissingPathsRule::default();
        let job = test_job(JobKindView::Build, "x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c1 = JobContext { job: &job, missing_count: Some(2), missing_nar_size: None, dependency_count: 0, queued_at: now };
        let c2 = JobContext { job: &job, missing_count: Some(10), missing_nar_size: None, dependency_count: 0, queued_at: now };

        assert!(rule.score(&c1, &w) > rule.score(&c2, &w));
    }

    #[test]
    fn missing_nar_size_smaller_wins() {
        let rule = MissingNarSizeRule::default();
        let job = test_job(JobKindView::Build, "x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c1 = JobContext { job: &job, missing_count: None, missing_nar_size: Some(1_048_576), dependency_count: 0, queued_at: now };
        let c2 = JobContext { job: &job, missing_count: None, missing_nar_size: Some(100_000_000), dependency_count: 0, queued_at: now };

        assert!(rule.score(&c1, &w) > rule.score(&c2, &w));
    }

    #[test]
    fn builtin_deprioritize_penalises_builtin() {
        let rule = BuiltinDeprioritizeRule::default();
        let real = test_job(JobKindView::Build, "x86_64-linux");
        let builtin = test_job(JobKindView::Build, "builtin");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c_real = JobContext { job: &real, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };
        let c_builtin = JobContext { job: &builtin, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };

        assert!(rule.score(&c_real, &w) > rule.score(&c_builtin, &w));
    }

    #[test]
    fn dependency_count_more_deps_wins() {
        let rule = DependencyCountRule::default();
        let job = test_job(JobKindView::Build, "x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c_few = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 1, queued_at: now };
        let c_many = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 20, queued_at: now };

        assert!(rule.score(&c_many, &w) > rule.score(&c_few, &w));
        assert!(rule.score(&c_few, &w) > 0.0);
    }

    #[test]
    fn dependency_count_zero_deps_zero_score() {
        let rule = DependencyCountRule::default();
        let job = test_job(JobKindView::Build, "x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let ctx = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };
        assert_eq!(rule.score(&ctx, &w), 0.0);
    }

    #[test]
    fn wait_time_longer_wait_scores_higher_but_capped() {
        let rule = WaitTimeRule::default();
        let job = test_job(JobKindView::Build, "x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let ctx_fresh = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };
        let ctx_mid = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now - chrono::Duration::seconds(60) };
        let ctx_ancient = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now - chrono::Duration::seconds(10_000) };

        let fresh = rule.score(&ctx_fresh, &w);
        let mid = rule.score(&ctx_mid, &w);
        let ancient = rule.score(&ctx_ancient, &w);

        assert!(fresh < mid, "older should score higher: {fresh} vs {mid}");
        let cap = rule.max_wait_secs * rule.bonus_per_second;
        assert!(ancient <= cap + 0.01, "ancient must be capped at {cap}, got {ancient}");
        assert!(ancient >= cap - 0.01, "ancient should reach cap, got {ancient}");
    }

    #[test]
    fn reserve_rule_penalizes_fetch_worker_for_cached_eval_only() {
        let rule = ReserveFetchWorkersRule::default();
        let cached_eval = test_job(JobKindView::Eval { fetch_flake: false }, "x86_64-linux");
        let fetch_eval = test_job(JobKindView::Eval { fetch_flake: true }, "x86_64-linux");
        let build = test_job(JobKindView::Build, "x86_64-linux");

        let archs: Vec<String> = vec![];
        let fetch_w = worker(&archs, true);
        let eval_w = worker(&archs, false);
        let now = gradient_core::types::now();

        let ctx_cached = JobContext { job: &cached_eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };
        let ctx_fetch = JobContext { job: &fetch_eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };
        let ctx_build = JobContext { job: &build, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now };

        assert!(rule.score(&ctx_cached, &fetch_w) < 0.0, "fetch worker penalized for cached eval");
        assert_eq!(rule.score(&ctx_cached, &eval_w), 0.0, "eval-only worker not penalized");
        assert_eq!(rule.score(&ctx_fetch, &fetch_w), 0.0, "fetch-only eval not penalized");
        assert_eq!(rule.score(&ctx_build, &fetch_w), 0.0, "build job not penalized");
    }
}
