/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug)]
pub struct ResourceFitRule {
    pub ram_overshoot_penalty: f64,
    pub cpu_affinity_bonus: f64,
    pub cpu_heavy_threshold_ms: u64,
    pub cpu_bonus_cap: f64,
}

impl Default for ResourceFitRule {
    fn default() -> Self {
        Self { ram_overshoot_penalty: 400.0, cpu_affinity_bonus: 50.0, cpu_heavy_threshold_ms: 60_000, cpu_bonus_cap: 2.0 }
    }
}

impl ScoreRule for ResourceFitRule {
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        let Some(m) = worker.metrics else { return 0.0 };
        let h = job.job.history();
        if h.samples == 0 {
            return 0.0;
        }

        let mut s = 0.0;
        if m.ram_free_mb > 0 && h.predicted_peak_ram_mb > m.ram_free_mb {
            let overshoot = (h.predicted_peak_ram_mb - m.ram_free_mb) as f64 / m.ram_free_mb as f64;
            s -= self.ram_overshoot_penalty * overshoot * (1.0 + h.oom_rate as f64);
        }
        if h.avg_cpu_time_ms > self.cpu_heavy_threshold_ms {
            s += self.cpu_affinity_bonus * ((m.cpu_core_score as f64 / 1000.0).min(self.cpu_bonus_cap));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, JobKindView, LazyProviders, ScoredJob, WorkerMetricsView};
    use gradient_core::types::ids::OrganizationId;

    fn job_with_history(h: HistoryPrediction) -> ScoredJob<'static> {
        let provider: &'static dyn Fn() -> HistoryPrediction = Box::leak(Box::new(move || h));
        ScoredJob::new(
            "test",
            OrganizationId::now_v7(),
            JobKindView::Build,
            "x86_64-linux",
            false,
            LazyProviders { closure_size: &|| None, history: provider },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>) -> JobContext<'a> {
        JobContext { job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: gradient_core::types::now() }
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

        let s_small = rule.score(&ctx(&small), &w);
        let s_large = rule.score(&ctx(&large), &w);
        assert!(s_small < 0.0);
        assert!(s_large < s_small, "larger overshoot must be more negative: {s_large} vs {s_small}");
    }

    #[test]
    fn higher_oom_rate_is_more_negative_for_same_overshoot() {
        let rule = ResourceFitRule::default();
        let w = worker_with(WorkerMetricsView { ram_free_mb: 1000, ..Default::default() });

        let low = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 2000, oom_rate: 0.0, samples: 5, ..Default::default() });
        let high = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 2000, oom_rate: 0.5, samples: 5, ..Default::default() });

        assert!(rule.score(&ctx(&high), &w) < rule.score(&ctx(&low), &w));
    }

    #[test]
    fn cpu_heavy_on_strong_worker_is_positive_and_capped() {
        let rule = ResourceFitRule::default();
        let heavy = job_with_history(HistoryPrediction { avg_cpu_time_ms: 120_000, samples: 5, ..Default::default() });

        let strong = worker_with(WorkerMetricsView { cpu_core_score: 1500, ..Default::default() });
        let monster = worker_with(WorkerMetricsView { cpu_core_score: 100_000, ..Default::default() });

        let s_strong = rule.score(&ctx(&heavy), &strong);
        let s_monster = rule.score(&ctx(&heavy), &monster);
        assert!(s_strong > 0.0);
        let cap = rule.cpu_affinity_bonus * rule.cpu_bonus_cap;
        assert!((s_monster - cap).abs() < 0.001, "cpu bonus must cap at {cap}, got {s_monster}");
    }

    #[test]
    fn no_samples_is_zero() {
        let rule = ResourceFitRule::default();
        let w = worker_with(WorkerMetricsView { ram_free_mb: 100, ..Default::default() });
        let job = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 9000, avg_cpu_time_ms: 999_999, samples: 0, ..Default::default() });
        assert_eq!(rule.score(&ctx(&job), &w), 0.0);
    }

    #[test]
    fn no_metrics_is_zero() {
        let rule = ResourceFitRule::default();
        let w = WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: None };
        let job = job_with_history(HistoryPrediction { predicted_peak_ram_mb: 9000, samples: 5, ..Default::default() });
        assert_eq!(rule.score(&ctx(&job), &w), 0.0);
    }
}
