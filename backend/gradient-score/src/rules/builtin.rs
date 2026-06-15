/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::{InstanceContext, JobKindContext};
use crate::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug)]
pub struct MissingPathsRule {
    pub cap: f64,
    pub k: f64,
    pub fallback_avg: f64,
}

impl Default for MissingPathsRule {
    fn default() -> Self {
        Self { cap: 200.0, k: 2.0, fallback_avg: 20.0 }
    }
}

impl ScoreRule for MissingPathsRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        match job.missing_count {
            None => 0.0,
            Some(n) => {
                let base = self.k * instance.missing_paths.w1h_or(self.fallback_avg);

                self.cap * (1.0 - (n as f64 / base).clamp(0.0, 1.0))
            }
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
                let baseline = self.k * instance.nar_size_mb.w1h_or(1024.0);

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
    pub cap: f64,
    pub k: f64,
    pub fallback_avg: f64,
}

impl Default for DependencyCountRule {
    fn default() -> Self {
        Self { cap: 50.0, k: 2.0, fallback_avg: 10.0 }
    }
}

impl ScoreRule for DependencyCountRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        if job.job.build().is_none() {
            return 0.0;
        }

        let base = self.k * instance.dependency_cnt.w1h_or(self.fallback_avg);

        self.cap * (job.dependency_count as f64 / base).clamp(0.0, 1.0)
    }
}

#[derive(Debug)]
pub struct WaitTimeRule {
    pub gain: f64,
    pub fallback_avg_secs: f64,
    pub cap: f64,
}

impl Default for WaitTimeRule {
    fn default() -> Self {
        Self { gain: 60.0, fallback_avg_secs: 60.0, cap: 4000.0 }
    }
}

impl ScoreRule for WaitTimeRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let now = gradient_types::now();
        let waited = (now - job.ready_at).num_seconds().max(0) as f64;
        let avg = instance.wait_secs.w1h_or(self.fallback_avg_secs);

        (self.gain * (waited / avg)).min(self.cap)
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
        instance: &InstanceContext,
    ) -> f64 {
        let cached_eval = matches!(job.job.kind(), JobKindContext::Eval(e) if !e.fetch_flake);
        if !(worker.fetch && cached_eval) {
            return 0.0;
        }

        let spare = if instance.total_workers > 0 {
            instance.idle_workers as f64 / instance.total_workers as f64
        } else {
            0.0
        };

        -self.penalty * (1.0 - spare).clamp(0.0, 1.0)
    }
}

#[derive(Debug)]
pub struct RescoreWaitRule {
    pub penalty: f64,
    pub max_rounds: u32,
}

impl Default for RescoreWaitRule {
    fn default() -> Self {
        Self { penalty: 1000.0, max_rounds: 4 }
    }
}

impl ScoreRule for RescoreWaitRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        if job.job.build().is_none() {
            return 0.0;
        }

        if job.missing_nar_size.is_none() && job.rescore_count < self.max_rounds {
            -self.penalty
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob};
    use gradient_types::ids::OrganizationId;

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
        ScoredJob::new_eval("test", OrganizationId::now_v7(), fetch_flake, Default::default())
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
        let now = gradient_types::now();
        let inst = crate::context::InstanceContext {
            missing_paths: crate::context::Windowed { w1h: 10.0, ..Default::default() },
            ..Default::default()
        };

        let scored = JobContext { job: &job, missing_count: Some(0), missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let unscored = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!((rule.score(&scored, &w, &inst) - 200.0).abs() < 1e-9);
        assert_eq!(rule.score(&unscored, &w, &inst), 0.0);
        assert!(rule.score(&scored, &w, &inst) > rule.score(&unscored, &w, &inst));
    }

    #[test]
    fn missing_paths_fewer_missing_wins() {
        let rule = MissingPathsRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_types::now();
        let inst = crate::context::InstanceContext {
            missing_paths: crate::context::Windowed { w1h: 10.0, ..Default::default() },
            ..Default::default()
        };

        let c1 = JobContext { job: &job, missing_count: Some(2), missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c2 = JobContext { job: &job, missing_count: Some(10), missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&c1, &w, &inst) > rule.score(&c2, &w, &inst));
        assert!(rule.score(&c1, &w, &inst) >= 0.0);
        assert!(rule.score(&c2, &w, &inst) >= 0.0);
    }

    #[test]
    fn missing_nar_size_bounded_bonus() {
        let rule = MissingNarSizeRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_types::now();
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
        let now = gradient_types::now();

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
        let now = gradient_types::now();
        // w1h=10 → base=20; dep=1 → 2.5, dep=15 → 37.5 (both below saturation)
        let inst = crate::context::InstanceContext {
            dependency_cnt: crate::context::Windowed { w1h: 10.0, ..Default::default() },
            ..Default::default()
        };

        let c_few = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 1, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let c_many = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 15, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&c_many, &w, &inst) > rule.score(&c_few, &w, &inst));
        assert!(rule.score(&c_few, &w, &inst) > 0.0);
    }

    #[test]
    fn dependency_count_zero_deps_zero_score() {
        let rule = DependencyCountRule::default();
        let build = build_job("x86_64-linux");
        let eval = eval_job(false);
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_types::now();
        let inst = crate::context::InstanceContext {
            dependency_cnt: crate::context::Windowed { w1h: 10.0, ..Default::default() },
            ..Default::default()
        };

        let ctx_zero = JobContext { job: &build, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_eval = JobContext { job: &eval, missing_count: None, missing_nar_size: None, dependency_count: 5, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert_eq!(rule.score(&ctx_zero, &w, &inst), 0.0);
        assert_eq!(rule.score(&ctx_eval, &w, &inst), 0.0);
    }

    #[test]
    fn dependency_count_capped_at_50() {
        let rule = DependencyCountRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_types::now();
        let inst = crate::context::InstanceContext {
            dependency_cnt: crate::context::Windowed { w1h: 10.0, ..Default::default() },
            ..Default::default()
        };

        let ctx_huge = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 100_000, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!(rule.score(&ctx_huge, &w, &inst) <= 50.0);
    }

    #[test]
    fn wait_time_longer_wait_scores_higher_but_capped() {
        let rule = WaitTimeRule::default();
        let job = build_job("x86_64-linux");
        let archs = vec!["x86_64-linux".to_string()];
        let w = worker(&archs, false);
        let now = gradient_types::now();

        let ctx_fresh = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_mid = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now - chrono::Duration::seconds(60), ready_at: now - chrono::Duration::seconds(60), org_work_share: None, rescore_count: 0 };
        let ctx_ancient = JobContext { job: &job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now - chrono::Duration::seconds(10_000), ready_at: now - chrono::Duration::seconds(10_000), org_work_share: None, rescore_count: 0 };

        let fresh = rule.score(&ctx_fresh, &w, &InstanceContext::default());
        let mid = rule.score(&ctx_mid, &w, &InstanceContext::default());
        let ancient = rule.score(&ctx_ancient, &w, &InstanceContext::default());

        assert!(fresh < mid, "older should score higher: {fresh} vs {mid}");
        assert!(mid < ancient, "even older should score higher: {mid} vs {ancient}");
        assert!(ancient <= rule.cap + 1.0, "ancient must be capped at {}, got {ancient}", rule.cap);
        assert!(ancient > 1140.0, "ancient must clear the soft-cap budget for anti-starvation, got {ancient}");
    }

    #[test]
    fn rescore_wait_blocks_build_until_threshold_but_never_eval() {
        let rule = RescoreWaitRule::default();
        let archs: Vec<String> = vec![];
        let w = worker(&archs, false);
        let inst = crate::context::InstanceContext::default();
        let n = gradient_types::now();
        let build = build_job("x86_64-linux");
        let eval = eval_job(false);

        let c_build_none_0 = JobContext { job: &build, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: n, ready_at: n, org_work_share: None, rescore_count: 0 };
        let c_build_none_4 = JobContext { job: &build, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: n, ready_at: n, org_work_share: None, rescore_count: 4 };
        let c_build_some_0 = JobContext { job: &build, missing_count: None, missing_nar_size: Some(10), dependency_count: 0, queued_at: n, ready_at: n, org_work_share: None, rescore_count: 0 };
        let c_eval_none_0 = JobContext { job: &eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: n, ready_at: n, org_work_share: None, rescore_count: 0 };

        assert_eq!(rule.score(&c_build_none_0, &w, &inst), -1000.0);
        assert_eq!(rule.score(&c_build_none_4, &w, &inst), 0.0);
        assert_eq!(rule.score(&c_build_some_0, &w, &inst), 0.0);
        assert_eq!(rule.score(&c_eval_none_0, &w, &inst), 0.0);
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
        let now = gradient_types::now();

        let inst_full = crate::context::InstanceContext { total_workers: 4, idle_workers: 0, ..Default::default() };
        let inst_idle = crate::context::InstanceContext { total_workers: 4, idle_workers: 4, ..Default::default() };

        let ctx_cached = JobContext { job: &cached_eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_fetch = JobContext { job: &fetch_eval, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };
        let ctx_build = JobContext { job: &build, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: now, ready_at: now, org_work_share: None, rescore_count: 0 };

        assert!((rule.score(&ctx_cached, &fetch_w, &inst_full) - (-300.0)).abs() < 1e-9, "full penalty when no idle workers");
        assert_eq!(rule.score(&ctx_cached, &fetch_w, &inst_idle), 0.0, "fully relaxed when all workers idle");
        assert_eq!(rule.score(&ctx_cached, &eval_w, &InstanceContext::default()), 0.0, "eval-only worker not penalized");
        assert_eq!(rule.score(&ctx_fetch, &fetch_w, &InstanceContext::default()), 0.0, "fetch-only eval not penalized");
        assert_eq!(rule.score(&ctx_build, &fetch_w, &InstanceContext::default()), 0.0, "build job not penalized");
    }
}
