/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::score::context::InstanceContext;
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};

/// Fixed-output derivations fetch from the network, so prefer faster-network
/// workers. Bonus scales linearly to `reference_mbps`, then caps.
#[derive(Debug)]
pub struct NetworkAffinityRule {
    pub bonus: f64,
    pub reference_mbps: f64,
}

impl Default for NetworkAffinityRule {
    fn default() -> Self {
        Self {
            bonus: crate::score::weights::NETWORK_AFFINITY_BONUS,
            reference_mbps: crate::score::weights::NETWORK_REFERENCE_MBPS,
        }
    }
}

impl ScoreRule for NetworkAffinityRule {
    fn name(&self) -> &'static str {
        "NetworkAffinityRule"
    }

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

        let reference = instance.network_mbps.w24h_or(self.reference_mbps);
        self.bonus * (net as f64 / reference).min(1.0)
    }

    fn description(&self) -> &'static str {
        "Steers fixed-output (network-fetching) derivations towards workers with faster measured network throughput."
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
        Self {
            bonus: crate::score::weights::DISK_AFFINITY_BONUS,
            heavy_threshold_bytes: crate::score::weights::DISK_HEAVY_THRESHOLD_BYTES,
            reference_mbps: crate::score::weights::DISK_REFERENCE_MBPS,
        }
    }
}

impl ScoreRule for DiskAffinityRule {
    fn name(&self) -> &'static str {
        "DiskAffinityRule"
    }

    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        if job.job.build().is_none() {
            return 0.0;
        }
        let h = job.build_history();
        let heavy_threshold = instance
            .disk_bytes
            .w24h_or(self.heavy_threshold_bytes as f64);
        if h.samples == 0 || (h.avg_disk_bytes as f64) < heavy_threshold {
            return 0.0;
        }

        let Some(disk) = worker.metrics.and_then(|m| m.disk_speed_mbps) else {
            return 0.0;
        };
        self.bonus * (disk as f64 / self.reference_mbps).min(1.0)
    }

    fn description(&self) -> &'static str {
        "Steers builds that have historically been disk-heavy towards workers with faster measured disk throughput."
    }
}

/// CPU-heavy builds (by history) prefer workers with faster cores than the
/// fleet mean and avoid slower ones. The swing grows with the build's work in
/// log2 steps over the heavy threshold, so a long build outweighs the transfer a
/// cache-warm but slower worker would save. Work is historical CPU time, or wall
/// time for history captured without CPU accounting.
#[derive(Debug)]
pub struct CpuAffinityRule {
    pub weight: f64,
    pub heaviness_cap: f64,
    pub heavy_threshold_ms: u64,
}

impl Default for CpuAffinityRule {
    fn default() -> Self {
        Self {
            weight: crate::score::weights::CPU_AFFINITY_WEIGHT,
            heaviness_cap: crate::score::weights::CPU_HEAVINESS_CAP,
            heavy_threshold_ms: crate::score::weights::CPU_HEAVY_THRESHOLD_MS,
        }
    }
}

impl CpuAffinityRule {
    fn heaviness(&self, work_ms: u64, instance: &InstanceContext) -> Option<f64> {
        let threshold = instance
            .cpu_time_ms
            .w1h_or(self.heavy_threshold_ms as f64)
            .max(1.0);
        let ratio = work_ms as f64 / threshold;
        (ratio >= 1.0).then(|| (ratio.log2() + 1.0).min(self.heaviness_cap))
    }
}

impl ScoreRule for CpuAffinityRule {
    fn name(&self) -> &'static str {
        "CpuAffinityRule"
    }

    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let h = job.build_history();
        let cores = worker.metrics.map_or(0, |m| m.cpu_core_score);
        let Some(fleet) = instance.cpu_core_score_mean.filter(|f| *f > 0.0) else {
            return 0.0;
        };
        if h.samples == 0 || cores == 0 {
            return 0.0;
        }

        let work_ms = if h.avg_cpu_time_ms > 0 {
            h.avg_cpu_time_ms
        } else {
            h.build_time_ms
        };
        let Some(heaviness) = self.heaviness(work_ms, instance) else {
            return 0.0;
        };

        let relative_speed = (cores as f64 / fleet - 1.0).clamp(-1.0, 1.0);
        self.weight * heaviness * relative_speed
    }

    fn description(&self) -> &'static str {
        "Steers builds that have historically been CPU-heavy towards workers with faster cores than the fleet average and away from slower ones, outweighing cache warmth for long builds."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::context::{HistoryPrediction, ScoredJob, WorkerMetricsView};
    use gradient_types::ids::ProjectId;

    fn job(is_fixed_output: bool, h: HistoryPrediction) -> ScoredJob<'static> {
        ScoredJob::new_build(
            "t",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            is_fixed_output,
            None,
            None,
            h,
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>) -> JobContext<'a> {
        JobContext {
            job,
            missing_count: None,
            missing_nar_size: None,
            outputs_present: false,
            dependency_count: 0,
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            project_work_share: None,
            prioritized: false,
            rescore_count: 0,
            now: gradient_types::now(),
        }
    }

    fn worker_with(metrics: WorkerMetricsView) -> WorkerContext<'static> {
        WorkerContext {
            architectures: &[],
            system_features: &[],
            fetch: false,
            metrics: Some(metrics),
        }
    }

    #[test]
    fn network_rule_prefers_fast_net_for_fod() {
        let rule = NetworkAffinityRule::default();
        let j = job(true, HistoryPrediction::default());
        let fast = worker_with(WorkerMetricsView {
            network_speed_mbps: Some(100.0),
            ..Default::default()
        });
        let slow = worker_with(WorkerMetricsView {
            network_speed_mbps: Some(10.0),
            ..Default::default()
        });
        assert!(
            rule.score(&ctx(&j), &fast, &InstanceContext::default())
                > rule.score(&ctx(&j), &slow, &InstanceContext::default())
        );
    }

    #[test]
    fn network_rule_zero_for_non_fod() {
        let rule = NetworkAffinityRule::default();
        let j = job(false, HistoryPrediction::default());
        let fast = worker_with(WorkerMetricsView {
            network_speed_mbps: Some(100.0),
            ..Default::default()
        });
        assert_eq!(
            rule.score(&ctx(&j), &fast, &InstanceContext::default()),
            0.0
        );
    }

    #[test]
    fn network_rule_zero_without_metric() {
        let rule = NetworkAffinityRule::default();
        let j = job(true, HistoryPrediction::default());
        let w = worker_with(WorkerMetricsView {
            network_speed_mbps: None,
            ..Default::default()
        });
        assert_eq!(rule.score(&ctx(&j), &w, &InstanceContext::default()), 0.0);
    }

    #[test]
    fn disk_rule_prefers_fast_disk_for_heavy_build() {
        let rule = DiskAffinityRule::default();
        let heavy = HistoryPrediction {
            avg_disk_bytes: 500 * 1_048_576,
            samples: 5,
            ..Default::default()
        };
        let j = job(false, heavy);
        let fast = worker_with(WorkerMetricsView {
            disk_speed_mbps: Some(500.0),
            ..Default::default()
        });
        let slow = worker_with(WorkerMetricsView {
            disk_speed_mbps: Some(50.0),
            ..Default::default()
        });
        assert!(
            rule.score(&ctx(&j), &fast, &InstanceContext::default())
                > rule.score(&ctx(&j), &slow, &InstanceContext::default())
        );
    }

    #[test]
    fn disk_rule_zero_for_light_build() {
        let rule = DiskAffinityRule::default();
        let light = HistoryPrediction {
            avg_disk_bytes: 1_048_576,
            samples: 5,
            ..Default::default()
        };
        let j = job(false, light);
        let fast = worker_with(WorkerMetricsView {
            disk_speed_mbps: Some(500.0),
            ..Default::default()
        });
        assert_eq!(
            rule.score(&ctx(&j), &fast, &InstanceContext::default()),
            0.0
        );
    }

    #[test]
    fn disk_rule_zero_without_history() {
        let rule = DiskAffinityRule::default();
        let j = job(
            false,
            HistoryPrediction {
                avg_disk_bytes: 999 * 1_048_576,
                samples: 0,
                ..Default::default()
            },
        );
        let fast = worker_with(WorkerMetricsView {
            disk_speed_mbps: Some(500.0),
            ..Default::default()
        });
        assert_eq!(
            rule.score(&ctx(&j), &fast, &InstanceContext::default()),
            0.0
        );
    }

    #[test]
    fn disk_heavy_uses_instance_threshold() {
        let rule = DiskAffinityRule::default();
        let j = job(
            false,
            HistoryPrediction {
                avg_disk_bytes: 50 * 1_048_576,
                samples: 5,
                ..Default::default()
            },
        );
        let fast = worker_with(WorkerMetricsView {
            disk_speed_mbps: Some(500.0),
            ..Default::default()
        });

        assert_eq!(
            rule.score(&ctx(&j), &fast, &InstanceContext::default()),
            0.0
        );

        let mut inst = InstanceContext::default();
        inst.disk_bytes.w24h = Some((10 * 1_048_576) as f64);
        assert!(rule.score(&ctx(&j), &fast, &inst) > 0.0);
    }

    fn cpu_history(avg_cpu_time_ms: u64) -> HistoryPrediction {
        HistoryPrediction {
            avg_cpu_time_ms,
            samples: 5,
            ..Default::default()
        }
    }

    fn cores(cpu_core_score: u32) -> WorkerContext<'static> {
        worker_with(WorkerMetricsView {
            cpu_core_score,
            ..Default::default()
        })
    }

    fn fleet(mean: f64) -> InstanceContext {
        InstanceContext {
            cpu_core_score_mean: Some(mean),
            ..Default::default()
        }
    }

    #[test]
    fn cpu_rule_attracts_heavy_builds_to_faster_than_fleet_workers() {
        let rule = CpuAffinityRule::default();
        let j = job(false, cpu_history(10 * 60_000));
        let inst = fleet(10_000.0);

        assert!(rule.score(&ctx(&j), &cores(15_000), &inst) > 0.0);
        assert!(rule.score(&ctx(&j), &cores(5_000), &inst) < 0.0);
        assert_eq!(rule.score(&ctx(&j), &cores(10_000), &inst), 0.0);
    }

    #[test]
    fn cpu_rule_weighs_heavier_builds_more_up_to_a_bound() {
        let rule = CpuAffinityRule::default();
        let inst = fleet(10_000.0);
        let fast = cores(1_000_000);
        let score = |ms| rule.score(&ctx(&job(false, cpu_history(ms))), &fast, &inst);

        assert!(score(4 * 60_000) > score(2 * 60_000));
        assert!(score(2 * 60_000) > score(60_000));
        assert_eq!(score(1_000 * 60_000), rule.weight * rule.heaviness_cap);
    }

    #[test]
    fn cpu_rule_ignores_light_builds() {
        let rule = CpuAffinityRule::default();
        let j = job(false, cpu_history(1_000));
        assert_eq!(rule.score(&ctx(&j), &cores(50_000), &fleet(10_000.0)), 0.0);
    }

    #[test]
    fn cpu_rule_needs_history_and_a_fleet_reference() {
        let rule = CpuAffinityRule::default();
        let unmeasured = job(
            false,
            HistoryPrediction {
                avg_cpu_time_ms: 10 * 60_000,
                samples: 0,
                ..Default::default()
            },
        );
        let heavy = job(false, cpu_history(10 * 60_000));

        assert_eq!(
            rule.score(&ctx(&unmeasured), &cores(50_000), &fleet(10_000.0)),
            0.0
        );
        assert_eq!(
            rule.score(&ctx(&heavy), &cores(50_000), &InstanceContext::default()),
            0.0
        );
        assert_eq!(rule.score(&ctx(&heavy), &cores(0), &fleet(10_000.0)), 0.0);
    }

    #[test]
    fn cpu_rule_heavy_threshold_follows_the_instance_average() {
        let rule = CpuAffinityRule::default();
        let j = job(false, cpu_history(30_000));
        let mut inst = fleet(10_000.0);
        assert_eq!(rule.score(&ctx(&j), &cores(15_000), &inst), 0.0);

        inst.cpu_time_ms.w1h = Some(10_000.0);
        assert!(rule.score(&ctx(&j), &cores(15_000), &inst) > 0.0);
    }

    #[test]
    fn cpu_rule_falls_back_to_build_time_without_cpu_samples() {
        let rule = CpuAffinityRule::default();
        let j = job(
            false,
            HistoryPrediction {
                build_time_ms: 10 * 60_000,
                samples: 5,
                ..Default::default()
            },
        );
        assert!(rule.score(&ctx(&j), &cores(15_000), &fleet(10_000.0)) > 0.0);
    }
}
