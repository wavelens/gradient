/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::rule::{JobContext, ScoreRule, WorkerContext};

/// Fixed-output derivations fetch from the network, so prefer faster-network
/// workers. Bonus scales linearly to `reference_mbps`, then caps.
#[derive(Debug)]
pub struct NetworkAffinityRule {
    pub bonus: f64,
    pub reference_mbps: f64,
}

impl Default for NetworkAffinityRule {
    fn default() -> Self {
        Self { bonus: 80.0, reference_mbps: 100.0 }
    }
}

impl ScoreRule for NetworkAffinityRule {
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        if !job.job.is_fixed_output {
            return 0.0;
        }
        let Some(net) = worker.metrics.and_then(|m| m.network_speed_mbps) else {
            return 0.0;
        };
        self.bonus * (net as f64 / self.reference_mbps).min(1.0)
    }
}

/// Disk-heavy builds (by history) prefer faster-disk workers. Bonus scales to
/// `reference_mbps`, then caps. Zero without history or a disk metric.
#[derive(Debug)]
pub struct DiskAffinityRule {
    pub bonus: f64,
    pub heavy_threshold_bytes: u64,
    pub reference_mbps: f64,
}

impl Default for DiskAffinityRule {
    fn default() -> Self {
        Self { bonus: 60.0, heavy_threshold_bytes: 100 * 1_048_576, reference_mbps: 500.0 }
    }
}

impl ScoreRule for DiskAffinityRule {
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        let h = job.job.history();
        if h.samples == 0 || h.avg_disk_bytes < self.heavy_threshold_bytes {
            return 0.0;
        }
        let Some(disk) = worker.metrics.and_then(|m| m.disk_speed_mbps) else {
            return 0.0;
        };
        self.bonus * (disk as f64 / self.reference_mbps).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, JobKindView, LazyProviders, ScoredJob, WorkerMetricsView};
    use gradient_core::types::ids::OrganizationId;

    fn job(is_fixed_output: bool, h: HistoryPrediction) -> ScoredJob<'static> {
        let provider: &'static dyn Fn() -> HistoryPrediction = Box::leak(Box::new(move || h));
        ScoredJob::new(
            "t",
            OrganizationId::now_v7(),
            JobKindView::Build,
            "x86_64-linux",
            false,
            is_fixed_output,
            LazyProviders { closure_size: &|| None, history: provider },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>) -> JobContext<'a> {
        JobContext { job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: gradient_core::types::now(), org_share: None }
    }

    fn worker_with(metrics: WorkerMetricsView) -> WorkerContext<'static> {
        WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: Some(metrics) }
    }

    #[test]
    fn network_rule_prefers_fast_net_for_fod() {
        let rule = NetworkAffinityRule::default();
        let j = job(true, HistoryPrediction::default());
        let fast = worker_with(WorkerMetricsView { network_speed_mbps: Some(100.0), ..Default::default() });
        let slow = worker_with(WorkerMetricsView { network_speed_mbps: Some(10.0), ..Default::default() });
        assert!(rule.score(&ctx(&j), &fast) > rule.score(&ctx(&j), &slow));
    }

    #[test]
    fn network_rule_zero_for_non_fod() {
        let rule = NetworkAffinityRule::default();
        let j = job(false, HistoryPrediction::default());
        let fast = worker_with(WorkerMetricsView { network_speed_mbps: Some(100.0), ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &fast), 0.0);
    }

    #[test]
    fn network_rule_zero_without_metric() {
        let rule = NetworkAffinityRule::default();
        let j = job(true, HistoryPrediction::default());
        let w = worker_with(WorkerMetricsView { network_speed_mbps: None, ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &w), 0.0);
    }

    #[test]
    fn disk_rule_prefers_fast_disk_for_heavy_build() {
        let rule = DiskAffinityRule::default();
        let heavy = HistoryPrediction { avg_disk_bytes: 500 * 1_048_576, samples: 5, ..Default::default() };
        let j = job(false, heavy);
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });
        let slow = worker_with(WorkerMetricsView { disk_speed_mbps: Some(50.0), ..Default::default() });
        assert!(rule.score(&ctx(&j), &fast) > rule.score(&ctx(&j), &slow));
    }

    #[test]
    fn disk_rule_zero_for_light_build() {
        let rule = DiskAffinityRule::default();
        let light = HistoryPrediction { avg_disk_bytes: 1_048_576, samples: 5, ..Default::default() };
        let j = job(false, light);
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &fast), 0.0);
    }

    #[test]
    fn disk_rule_zero_without_history() {
        let rule = DiskAffinityRule::default();
        let j = job(false, HistoryPrediction { avg_disk_bytes: 999 * 1_048_576, samples: 0, ..Default::default() });
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &fast), 0.0);
    }
}
