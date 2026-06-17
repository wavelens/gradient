/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::InstanceContext;
use crate::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug)]
pub struct ResourceFitRule {
    pub ram_overshoot_penalty: f64,
    pub max_overshoot: f64,
    pub cpu_affinity_bonus: f64,
    pub cpu_heavy_threshold_ms: u64,
    pub cpu_bonus_cap: f64,
}

impl Default for ResourceFitRule {
    fn default() -> Self {
        Self { ram_overshoot_penalty: 400.0, max_overshoot: 2.0, cpu_affinity_bonus: 50.0, cpu_heavy_threshold_ms: 60_000, cpu_bonus_cap: 2.0 }
    }
}

impl ScoreRule for ResourceFitRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let Some(m) = worker.metrics else { return 0.0 };
        let h = job.job.history();
        if h.samples == 0 {
            return 0.0;
        }

        let mut s = 0.0;
        if m.ram_free_mb > 0 && h.predicted_peak_ram_mb > m.ram_free_mb {
            let overshoot = ((h.predicted_peak_ram_mb - m.ram_free_mb) as f64 / m.ram_free_mb as f64).min(self.max_overshoot);
            // bounded so WaitTime can overcome it; scaled by per-job and instance-wide oom trend
            s -= self.ram_overshoot_penalty * overshoot * (1.0 + h.oom_rate as f64) * (1.0 + instance.oom_rate.w1h);
        }

        let cpu_threshold = instance.cpu_time_ms.w1h_or(self.cpu_heavy_threshold_ms as f64);
        if (h.avg_cpu_time_ms as f64) > cpu_threshold {
            s += self.cpu_affinity_bonus * ((m.cpu_core_score as f64 / 1000.0).min(self.cpu_bonus_cap));
        }
        s
    }

    fn description(&self) -> &'static str {
        "Uses historical RAM and CPU usage to penalize workers that would likely run out of memory and reward CPU-strong workers for compute-heavy builds."
    }
}

/// Hard penalty for dispatching real work to a worker whose CPU or RAM is
/// already saturated. Substitute-only `builtin` jobs fetch rather than build, so
/// they load the worker less and get a more lenient CPU threshold, but a
/// RAM-starved worker can still fail a fetch, so RAM saturation still applies.
#[derive(Debug)]
pub struct ResourceSaturationRule {
    pub penalty: f64,
    pub cpu_saturated_pct: f32,
    pub cpu_saturated_pct_builtin: f32,
    pub ram_saturated_free_frac: f64,
    pub ram_fit_headroom: f64,
}

impl Default for ResourceSaturationRule {
    fn default() -> Self {
        Self {
            penalty: 1000.0,
            cpu_saturated_pct: 80.0,
            cpu_saturated_pct_builtin: 90.0,
            ram_saturated_free_frac: 0.10,
            ram_fit_headroom: 1.1,
        }
    }
}

impl ScoreRule for ResourceSaturationRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        let Some(m) = worker.metrics else { return 0.0 };

        // `builtin` is a substitute-only fetch (lighter CPU load); evals have no
        // architecture and their own rules.
        let Some(b) = job.job.build() else { return 0.0 };
        let cpu_saturated_pct = if b.architecture == "builtin" {
            self.cpu_saturated_pct_builtin
        } else {
            self.cpu_saturated_pct
        };

        let mut s = 0.0;

        // The worker is already saturated.
        let cpu_saturated = m.cpu_usage_pct >= cpu_saturated_pct;
        let ram_saturated = m.ram_total_mb > 0
            && (m.ram_free_mb as f64 / m.ram_total_mb as f64) <= self.ram_saturated_free_frac;
        if cpu_saturated || ram_saturated {
            s -= self.penalty;
        }

        // The build's historical peak RAM (plus headroom) would not fit in the
        // worker's free RAM, so it would likely OOM here.
        let h = job.job.history();
        if h.samples > 0
            && m.ram_free_mb > 0
            && h.predicted_peak_ram_mb as f64 * self.ram_fit_headroom > m.ram_free_mb as f64
        {
            s -= self.penalty;
        }

        s
    }

    fn description(&self) -> &'static str {
        "Strongly penalizes sending a real build to a worker whose CPU or RAM is already saturated, or whose free RAM cannot hold the build's historical peak RAM plus headroom; substitute-only builtin fetches get a more lenient CPU threshold but are still RAM-aware."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob, Windowed, WorkerMetricsView};
    use gradient_types::ids::OrganizationId;

    fn job_with_history(h: HistoryPrediction) -> ScoredJob<'static> {
        let provider: &'static dyn Fn() -> HistoryPrediction = Box::leak(Box::new(move || h));
        ScoredJob::new_build(
            "test",
            OrganizationId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            LazyProviders { closure_size: &|| None, history: provider },
        )
    }

    fn eval_job_with_history(h: HistoryPrediction) -> ScoredJob<'static> {
        ScoredJob::new_eval("eval", OrganizationId::now_v7(), true, h)
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>) -> JobContext<'a> {
        JobContext { job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: gradient_types::now(), ready_at: gradient_types::now(), org_work_share: None, rescore_count: 0 }
    }

    fn worker_with(metrics: WorkerMetricsView) -> WorkerContext<'static> {
        WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: Some(metrics) }
    }

    #[test]
    fn ram_overshoot_is_negative_and_scales_with_overshoot() {
        let rule = ResourceFitRule::default();
        let m = WorkerMetricsView { ram_free_mb: 1000, ..Default::default() };
        let w = worker_with(m);

        let small = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 1500, samples: 5, ..Default::default() });
        let large = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 3000, samples: 5, ..Default::default() });

        let s_small = rule.score(&ctx(&small), &w, &InstanceContext::default());
        let s_large = rule.score(&ctx(&large), &w, &InstanceContext::default());
        assert!(s_small < 0.0);
        assert!(s_large < s_small, "larger overshoot must be more negative: {s_large} vs {s_small}");
    }

    #[test]
    fn ram_overshoot_penalty_is_bounded() {
        let rule = ResourceFitRule::default();
        let w = worker_with(WorkerMetricsView { ram_free_mb: 100, ..Default::default() });
        let job = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 1_000_000, oom_rate: 1.0, samples: 5, ..Default::default() });
        let inst = InstanceContext { oom_rate: Windowed { w1h: 1.0, ..Default::default() }, ..Default::default() };

        let s = rule.score(&ctx(&job), &w, &inst);
        assert!(s >= -(rule.ram_overshoot_penalty * rule.max_overshoot * 4.0) - 0.001, "penalty must be bounded by clamp, got {s}");
        assert!(s > -4000.0, "penalty must stay below WaitTimeRule cap so wait can overcome it, got {s}");
    }

    #[test]
    fn higher_oom_rate_is_more_negative_for_same_overshoot() {
        let rule = ResourceFitRule::default();
        let w = worker_with(WorkerMetricsView { ram_free_mb: 1000, ..Default::default() });

        let low = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 2000, oom_rate: 0.0, samples: 5, ..Default::default() });
        let high = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 2000, oom_rate: 0.5, samples: 5, ..Default::default() });

        assert!(rule.score(&ctx(&high), &w, &InstanceContext::default()) < rule.score(&ctx(&low), &w, &InstanceContext::default()));
    }

    #[test]
    fn cpu_heavy_on_strong_worker_is_positive_and_capped() {
        let rule = ResourceFitRule::default();
        let heavy = job_with_history(HistoryPrediction { avg_cpu_time_ms: 120_000, samples: 5, ..Default::default() });

        let strong = worker_with(WorkerMetricsView { cpu_core_score: 1500, ..Default::default() });
        let monster = worker_with(WorkerMetricsView { cpu_core_score: 100_000, ..Default::default() });

        let s_strong = rule.score(&ctx(&heavy), &strong, &InstanceContext::default());
        let s_monster = rule.score(&ctx(&heavy), &monster, &InstanceContext::default());
        assert!(s_strong > 0.0);
        let cap = rule.cpu_affinity_bonus * rule.cpu_bonus_cap;
        assert!((s_monster - cap).abs() < 0.001, "cpu bonus must cap at {cap}, got {s_monster}");
    }

    #[test]
    fn eval_ram_overshoot_routes_to_big_ram_worker() {
        let rule = ResourceFitRule::default();
        let job = || eval_job_with_history(HistoryPrediction { predicted_peak_ram_mb: 40_000, samples: 5, ..Default::default() });
        let small = worker_with(WorkerMetricsView { ram_free_mb: 16_000, ..Default::default() });
        let big = worker_with(WorkerMetricsView { ram_free_mb: 64_000, ..Default::default() });
        assert!(rule.score(&ctx(&job()), &small, &InstanceContext::default()) < 0.0);
        assert_eq!(rule.score(&ctx(&job()), &big, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn no_samples_is_zero() {
        let rule = ResourceFitRule::default();
        let w = worker_with(WorkerMetricsView { ram_free_mb: 100, ..Default::default() });
        let job = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 9000, avg_cpu_time_ms: 999_999, samples: 0, ..Default::default() });
        assert_eq!(rule.score(&ctx(&job), &w, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn no_metrics_is_zero() {
        let rule = ResourceFitRule::default();
        let w = WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: None };
        let job = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 9000, samples: 5, ..Default::default() });
        assert_eq!(rule.score(&ctx(&job), &w, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn cpu_bonus_triggers_below_fixed_threshold_when_instance_avg_low() {
        let rule = ResourceFitRule::default();
        let strong = worker_with(WorkerMetricsView { cpu_core_score: 1500, ..Default::default() });
        let job = job_with_history(HistoryPrediction { avg_cpu_time_ms: 30_000, samples: 5, ..Default::default() });

        assert_eq!(rule.score(&ctx(&job), &strong, &InstanceContext::default()), 0.0);

        let mut inst = InstanceContext::default();
        inst.cpu_time_ms.w1h = 10_000.0;
        assert!(rule.score(&ctx(&job), &strong, &inst) > 0.0);
    }

    fn builtin_job() -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test",
            OrganizationId::now_v7(),
            "builtin",
            false,
            false,
            None,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        )
    }

    #[test]
    fn saturation_penalizes_real_build_on_hot_cpu_or_ram_only() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction::default()); // x86_64-linux

        let cpu_hot = worker_with(WorkerMetricsView {
            cpu_usage_pct: 95.0,
            ram_total_mb: 16_000,
            ram_free_mb: 8_000,
            ..Default::default()
        });
        let ram_hot = worker_with(WorkerMetricsView {
            cpu_usage_pct: 10.0,
            ram_total_mb: 16_000,
            ram_free_mb: 800,
            ..Default::default()
        });
        let idle = worker_with(WorkerMetricsView {
            cpu_usage_pct: 10.0,
            ram_total_mb: 16_000,
            ram_free_mb: 8_000,
            ..Default::default()
        });

        assert_eq!(rule.score(&ctx(&job), &cpu_hot, &InstanceContext::default()), -1000.0);
        assert_eq!(rule.score(&ctx(&job), &ram_hot, &InstanceContext::default()), -1000.0);
        assert_eq!(rule.score(&ctx(&job), &idle, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn saturation_is_lenient_for_builtin_and_exempts_evals_and_no_metrics() {
        let rule = ResourceSaturationRule::default();

        // CPU between the real-build (80%) and builtin (90%) thresholds, RAM roomy:
        // the lighter builtin fetch is spared, a real build is penalized.
        let warm = WorkerMetricsView {
            cpu_usage_pct: 85.0,
            ram_total_mb: 16_000,
            ram_free_mb: 8_000,
            ..Default::default()
        };
        assert_eq!(rule.score(&ctx(&builtin_job()), &worker_with(warm), &InstanceContext::default()), 0.0);
        let real = job_with_history(HistoryPrediction::default());
        assert_eq!(rule.score(&ctx(&real), &worker_with(warm), &InstanceContext::default()), -1000.0);

        // Evals (no architecture) and no-metrics workers are fully exempt even on a hot worker.
        let hot = WorkerMetricsView {
            cpu_usage_pct: 99.0,
            ram_total_mb: 16_000,
            ram_free_mb: 100,
            ..Default::default()
        };
        let eval = eval_job_with_history(HistoryPrediction::default());
        assert_eq!(rule.score(&ctx(&eval), &worker_with(hot), &InstanceContext::default()), 0.0);

        let no_metrics =
            WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: None };
        assert_eq!(rule.score(&ctx(&real), &no_metrics, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn ram_prediction_exceeding_free_penalizes_and_stacks_with_saturation() {
        let rule = ResourceSaturationRule::default();
        let job =
            job_with_history(HistoryPrediction { predicted_peak_ram_mb: 10_000, samples: 5, ..Default::default() });

        // Not saturated, but 10_000 * 1.1 = 11_000 > 8_000 free -> RAM won't fit.
        let tight = worker_with(WorkerMetricsView {
            cpu_usage_pct: 10.0,
            ram_total_mb: 16_000,
            ram_free_mb: 8_000,
            ..Default::default()
        });
        assert_eq!(rule.score(&ctx(&job), &tight, &InstanceContext::default()), -1000.0);

        // 12_000 free >= 11_000 needed and not saturated -> no penalty.
        let roomy = worker_with(WorkerMetricsView {
            cpu_usage_pct: 10.0,
            ram_total_mb: 32_000,
            ram_free_mb: 12_000,
            ..Default::default()
        });
        assert_eq!(rule.score(&ctx(&job), &roomy, &InstanceContext::default()), 0.0);

        // Saturated CPU AND RAM won't fit -> both -1000 penalties stack.
        let hot_and_tight = worker_with(WorkerMetricsView {
            cpu_usage_pct: 99.0,
            ram_total_mb: 16_000,
            ram_free_mb: 8_000,
            ..Default::default()
        });
        assert_eq!(rule.score(&ctx(&job), &hot_and_tight, &InstanceContext::default()), -2000.0);
    }

    #[test]
    fn ram_overshoot_more_negative_with_high_instance_oom() {
        let rule = ResourceFitRule::default();
        let w = worker_with(WorkerMetricsView { ram_free_mb: 1000, ..Default::default() });
        let job = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 2000, samples: 5, ..Default::default() });

        let mut low = InstanceContext::default();
        low.oom_rate.w1h = 0.0;
        let mut high = InstanceContext::default();
        high.oom_rate.w1h = 1.0;

        assert!(rule.score(&ctx(&job), &w, &low) > rule.score(&ctx(&job), &w, &high));
    }
}
