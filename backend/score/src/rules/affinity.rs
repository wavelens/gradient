/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::InstanceContext;
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
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let Some(b) = job.job.build() else { return 0.0 };
        if !b.is_fixed_output {
            return 0.0;
        }

        let Some(net) = worker.metrics.and_then(|m| m.network_speed_mbps) else {
            return 0.0;
        };

        let reference = if instance.network_mbps.w24h > 0.0 {
            instance.network_mbps.w24h
        } else {
            self.reference_mbps
        };
        self.bonus * (net as f64 / reference).min(1.0)
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
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let Some(b) = job.job.build() else { return 0.0 };
        let h = b.history();
        let heavy_threshold = if instance.disk_bytes.w24h > 0.0 {
            instance.disk_bytes.w24h
        } else {
            self.heavy_threshold_bytes as f64
        };
        if h.samples == 0 || (h.avg_disk_bytes as f64) < heavy_threshold {
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
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob, WorkerMetricsView};
    use gradient_core::types::ids::OrganizationId;

    fn job(is_fixed_output: bool, h: HistoryPrediction) -> ScoredJob<'static> {
        let provider: &'static dyn Fn() -> HistoryPrediction = Box::leak(Box::new(move || h));
        ScoredJob::new_build(
            "t",
            OrganizationId::now_v7(),
            "x86_64-linux",
            false,
            is_fixed_output,
            None,
            LazyProviders { closure_size: &|| None, history: provider },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>) -> JobContext<'a> {
        JobContext { job, missing_count: None, missing_nar_size: None, dependency_count: 0, queued_at: gradient_core::types::now(), ready_at: gradient_core::types::now(), org_work_share: None, rescore_count: 0 }
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
        assert!(rule.score(&ctx(&j), &fast, &InstanceContext::default()) > rule.score(&ctx(&j), &slow, &InstanceContext::default()));
    }

    #[test]
    fn network_rule_zero_for_non_fod() {
        let rule = NetworkAffinityRule::default();
        let j = job(false, HistoryPrediction::default());
        let fast = worker_with(WorkerMetricsView { network_speed_mbps: Some(100.0), ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &fast, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn network_rule_zero_without_metric() {
        let rule = NetworkAffinityRule::default();
        let j = job(true, HistoryPrediction::default());
        let w = worker_with(WorkerMetricsView { network_speed_mbps: None, ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &w, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn disk_rule_prefers_fast_disk_for_heavy_build() {
        let rule = DiskAffinityRule::default();
        let heavy = HistoryPrediction { avg_disk_bytes: 500 * 1_048_576, samples: 5, ..Default::default() };
        let j = job(false, heavy);
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });
        let slow = worker_with(WorkerMetricsView { disk_speed_mbps: Some(50.0), ..Default::default() });
        assert!(rule.score(&ctx(&j), &fast, &InstanceContext::default()) > rule.score(&ctx(&j), &slow, &InstanceContext::default()));
    }

    #[test]
    fn disk_rule_zero_for_light_build() {
        let rule = DiskAffinityRule::default();
        let light = HistoryPrediction { avg_disk_bytes: 1_048_576, samples: 5, ..Default::default() };
        let j = job(false, light);
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &fast, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn disk_rule_zero_without_history() {
        let rule = DiskAffinityRule::default();
        let j = job(false, HistoryPrediction { avg_disk_bytes: 999 * 1_048_576, samples: 0, ..Default::default() });
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });
        assert_eq!(rule.score(&ctx(&j), &fast, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn disk_heavy_uses_instance_threshold() {
        let rule = DiskAffinityRule::default();
        let j = job(false, HistoryPrediction { avg_disk_bytes: 50 * 1_048_576, samples: 5, ..Default::default() });
        let fast = worker_with(WorkerMetricsView { disk_speed_mbps: Some(500.0), ..Default::default() });

        assert_eq!(rule.score(&ctx(&j), &fast, &InstanceContext::default()), 0.0);

        let mut inst = InstanceContext::default();
        inst.disk_bytes.w24h = (10 * 1_048_576) as f64;
        assert!(rule.score(&ctx(&j), &fast, &inst) > 0.0);
    }
}
