/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::{InstanceContext, JobKindContext};
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
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        match job.missing_count {
            None => 0.0,
            Some(n) => self.scored_bonus - (n as f64) * self.path_penalty,
        }
    }
}

#[derive(Debug)]
pub struct MissingNarSizeRule {
    pub cap: f64,
    pub k: f64,
}

impl Default for MissingNarSizeRule {
    fn default() -> Self {
        Self { cap: 500.0, k: 2.0 }
    }
}

impl ScoreRule for MissingNarSizeRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        match job.missing_nar_size {
            None => 0.0,
            Some(0) => self.cap,
            Some(b) => {
                let mb = b as f64 / 1_048_576.0;
                let baseline = if instance.nar_size_mb.w1h > 0.0 {
                    self.k * instance.nar_size_mb.w1h
                } else {
                    self.k * 1024.0
                };

                self.cap * (1.0 - (mb / baseline).clamp(0.0, 1.0))
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
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        if let Some(b) = job.job.build()
            && b.architecture == "builtin"
        {
            return -self.penalty;
        }

        0.0
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
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        if job.job.build().is_some() {
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
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
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
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        match job.job.kind() {
            JobKindContext::Eval(e) if worker.fetch && !e.fetch_flake => -self.penalty,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob};
    use gradient_core::types::ids::OrganizationId;

    fn build_job(arch: &'static str) -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test",
            OrganizationId::now_v7(),
            arch,
            false,
            false,
            None,
            LazyProviders {
                closure_size: &|| None,
                history: &|| HistoryPrediction::default(),
            },
        )
    }

    fn eval_job(fetch_flake: bool) -> ScoredJob<'static> {
        ScoredJob::new_eval("test", OrganizationId::now_v7(), fetch_flake)
    }

    fn worker<'a>(archs: &'a [String], fetch: bool) -> WorkerContext<'a> {
        WorkerContext { architectures: archs, system_features: &[], fetch, metrics: None }
    }

    #[test]
    fn missing_paths_scored_zero_wins_over_unscored() {
        let rule = MissingPathsRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let scored = JobContext { job: &job, missing_count: Some(0), missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let unscored = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&scored, &w, &InstanceContext::default()) > rule.score(&unscored, &w, &InstanceContext::default()));
    }

    #[test]
    fn missing_paths_fewer_missing_wins() {
        let rule = MissingPathsRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c1 = JobContext { job: &job, missing_count: Some(2), missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c2 = JobContext { job: &job, missing_count: Some(10), missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&c1, &w, &InstanceContext::default()) > rule.score(&c2, &w, &InstanceContext::default()));
    }

    #[test]
    fn missing_nar_size_bounded_bonus() {
        let rule = MissingNarSizeRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();
        let inst = crate::context::InstanceContext {
            nar_size_mb: crate::context::Windowed { w1h: 100.0, ..Default::default() },
            ..Default::default()
        };

        let c_none = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c_zero = JobContext { job: &job, missing_count: None, missing_nar_size: Some(0), dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c_huge = JobContext { job: &job, missing_count: None, missing_nar_size: Some(100_000_000_000), dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert_eq!(rule.score(&c_none, &w, &inst), 0.0);
        assert!((rule.score(&c_zero, &w, &inst) - 500.0).abs() < 1e-9);
        assert!(rule.score(&c_huge, &w, &inst) >= 0.0);
        assert!(rule.score(&c_zero, &w, &inst) > rule.score(&c_huge, &w, &inst));
    }

    #[test]
    fn builtin_deprioritize_penalises_builtin() {
        let rule = BuiltinDeprioritizeRule::default();
        let real = build_job("x86_64-linux");
        let builtin = build_job("builtin");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c_real = JobContext { job: &real, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c_builtin = JobContext { job: &builtin, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&c_real, &w, &InstanceContext::default()) > rule.score(&c_builtin, &w, &InstanceContext::default()));
    }

    #[test]
    fn dependency_count_more_deps_wins() {
        let rule = DependencyCountRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let c_few = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 1, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c_many = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 20, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&c_many, &w, &InstanceContext::default()) > rule.score(&c_few, &w, &InstanceContext::default()));
        assert!(rule.score(&c_few, &w, &InstanceContext::default()) > 0.0);
    }

    #[test]
    fn dependency_count_zero_deps_zero_score() {
        let rule = DependencyCountRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let ctx = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        assert_eq!(rule.score(&ctx, &w, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn wait_time_longer_wait_scores_higher_but_capped() {
        let rule = WaitTimeRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_core::types::now();

        let ctx_fresh = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_mid = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now - chrono::Duration::seconds(60), ready_at: now - chrono::Duration::seconds(60), org_work_share: None, rescore_count: 0 };
        let ctx_ancient = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now - chrono::Duration::seconds(10_000), ready_at: now - chrono::Duration::seconds(10_000), org_work_share: None, rescore_count: 0 };

        let fresh = rule.score(&ctx_fresh, &w, &InstanceContext::default());
        let mid = rule.score(&ctx_mid, &w, &InstanceContext::default());
        let ancient = rule.score(&ctx_ancient, &w, &InstanceContext::default());

        assert!(fresh < mid, "older should score higher: {fresh} vs {mid}");
        let cap = rule.max_wait_secs * rule.bonus_per_second;
        assert!(ancient <= cap + 0.01, "ancient must be capped at {cap}, got {ancient}");
        assert!(ancient >= cap - 0.01, "ancient should reach cap, got {ancient}");
    }

    #[test]
    fn reserve_rule_penalizes_fetch_worker_for_cached_eval_only() {
        let rule = ReserveFetchWorkersRule::default();
        let cached_eval = eval_job(false);
        let fetch_eval = eval_job(true);
        let build = build_job("x86_64-linux");

        let archs: Vec<String> = vec![];
        let fetch_w = worker(&archs, true);
        let eval_w = worker(&archs, false);
        let now = gradient_core::types::now();

        let ctx_cached = JobContext { job: &cached_eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_fetch = JobContext { job: &fetch_eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_build = JobContext { job: &build, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&ctx_cached, &fetch_w, &InstanceContext::default()) < 0.0, "fetch worker penalized for cached eval");
        assert_eq!(rule.score(&ctx_cached, &eval_w, &InstanceContext::default()), 0.0, "eval-only worker not penalized");
        assert_eq!(rule.score(&ctx_fetch, &fetch_w, &InstanceContext::default()), 0.0, "fetch-only eval not penalized");
        assert_eq!(rule.score(&ctx_build, &fetch_w, &InstanceContext::default()), 0.0, "build job not penalized");
    }
}
