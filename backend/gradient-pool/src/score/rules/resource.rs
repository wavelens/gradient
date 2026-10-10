/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::score::context::{InstanceContext, WorkerMetricsView};
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RamFit {
    Fits,
    FitsOnceRamIsReleased,
    DoesNotFit,
}

/// Substitute-only `builtin` jobs are getting a more lenient CPU threshold. RAM saturation is still
/// applying because a RAM-starved worker can fail a fetch too.
#[derive(Debug)]
pub struct ResourceSaturationRule {
    pub penalty: f64,
    pub cpu_saturated_pct: f32,
    pub cpu_saturated_pct_builtin: f32,
    pub ram_saturated_free_frac: f64,
}

impl Default for ResourceSaturationRule {
    fn default() -> Self {
        Self {
            penalty: crate::score::weights::RESOURCE_SATURATION_PENALTY,
            cpu_saturated_pct: crate::score::weights::CPU_SATURATED_PCT as f32,
            cpu_saturated_pct_builtin: crate::score::weights::CPU_SATURATED_PCT_BUILTIN as f32,
            ram_saturated_free_frac: crate::score::weights::RAM_SATURATED_FREE_FRAC,
        }
    }
}

impl ResourceSaturationRule {
    pub const NAME: &'static str = "ResourceSaturationRule";

    fn ram_fit(job: &JobContext<'_>, m: &WorkerMetricsView, instance: &InstanceContext) -> RamFit {
        let needed = job
            .ram_need()
            .needed_to_start_mb(instance.unmeasured_build_ram_mb());
        if needed == 0 || m.ram_available_mb().is_none_or(|free| needed <= free) {
            RamFit::Fits
        } else if m.will_release_ram() && needed <= m.ram_total_mb {
            RamFit::FitsOnceRamIsReleased
        } else {
            RamFit::DoesNotFit
        }
    }
}

impl ScoreRule for ResourceSaturationRule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let Some(m) = worker.metrics else { return 0.0 };

        let Some(b) = job.job.build() else { return 0.0 };
        let cpu_saturated_pct = if b.architecture == gradient_types::BUILTIN_ARCH {
            self.cpu_saturated_pct_builtin
        } else {
            self.cpu_saturated_pct
        };

        let mut s = 0.0;

        // Absent pre-heartbeat samples are never counting as saturated.
        let cpu_saturated = m.cpu_usage_pct.is_some_and(|c| c >= cpu_saturated_pct);
        let ram_saturated = m.ram_total_mb > 0
            && m.ram_free_mb.is_some_and(|f| {
                (f as f64 / m.ram_total_mb as f64) <= self.ram_saturated_free_frac
            });
        if cpu_saturated || ram_saturated {
            s -= self.penalty;
        }

        if Self::ram_fit(job, &m, instance) != RamFit::Fits {
            s -= self.penalty;
        }

        s
    }

    /// A hold needs reserved RAM the worker is going to release. A build larger than the whole
    /// worker, or short of RAM on a worker without reservations, is only penalized.
    fn veto(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> bool {
        worker
            .metrics
            .is_some_and(|m| Self::ram_fit(job, &m, instance) == RamFit::FitsOnceRamIsReleased)
    }

    fn description(&self) -> &'static str {
        "Strongly penalizes sending a real build to a worker whose CPU or RAM is already saturated. Holds a build while the worker's free RAM, less the RAM reserved for the jobs already assigned to it and for a held build waiting for it, cannot take the build's historical peak RAM plus headroom. A prioritized build, or a build held for 60 seconds, keeps its RAM free on a single worker and starts there first. A build without history counts as the mean peak RAM of the instance's builds. Substitute-only builtin fetches get a more lenient CPU threshold."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::context::{HistoryPrediction, ScoredJob, WorkerMetricsView};
    use crate::score::weights::RESOURCE_SATURATION_PENALTY as PENALTY;
    use gradient_types::ids::ProjectId;

    fn job_with_history(h: HistoryPrediction) -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            None,
            h,
        )
    }

    fn eval_job_with_history(h: HistoryPrediction) -> ScoredJob<'static> {
        ScoredJob::new_eval("eval", ProjectId::now_v7(), true, h)
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>) -> JobContext<'a> {
        JobContext {
            job,
            ..Default::default()
        }
    }

    fn worker_with(metrics: WorkerMetricsView) -> WorkerContext<'static> {
        WorkerContext {
            metrics: Some(metrics),
            ..Default::default()
        }
    }

    fn builtin_job() -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test",
            ProjectId::now_v7(),
            "builtin",
            false,
            false,
            None,
            None,
            HistoryPrediction::default(),
        )
    }

    #[test]
    fn saturation_penalizes_real_build_on_hot_cpu_or_ram_only() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction::default());

        let cpu_hot = worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(95.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(8_000),
            ..Default::default()
        });
        let ram_hot = worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(10.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(800),
            ..Default::default()
        });
        let idle = worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(10.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(8_000),
            ..Default::default()
        });

        assert_eq!(
            rule.score(&ctx(&job), &cpu_hot, &InstanceContext::default()),
            -PENALTY
        );
        assert_eq!(
            rule.score(&ctx(&job), &ram_hot, &InstanceContext::default()),
            -PENALTY
        );
        assert_eq!(
            rule.score(&ctx(&job), &idle, &InstanceContext::default()),
            0.0
        );
    }

    #[test]
    fn saturation_is_lenient_for_builtin_and_exempts_evals_and_no_metrics() {
        let rule = ResourceSaturationRule::default();

        let warm = WorkerMetricsView {
            cpu_usage_pct: Some(85.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(8_000),
            ..Default::default()
        };
        assert_eq!(
            rule.score(
                &ctx(&builtin_job()),
                &worker_with(warm),
                &InstanceContext::default()
            ),
            0.0
        );
        let real = job_with_history(HistoryPrediction::default());
        assert_eq!(
            rule.score(&ctx(&real), &worker_with(warm), &InstanceContext::default()),
            -PENALTY
        );

        let hot = WorkerMetricsView {
            cpu_usage_pct: Some(99.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(100),
            ..Default::default()
        };
        let eval = eval_job_with_history(HistoryPrediction::default());
        assert_eq!(
            rule.score(&ctx(&eval), &worker_with(hot), &InstanceContext::default()),
            0.0
        );

        let no_metrics = WorkerContext::default();
        assert_eq!(
            rule.score(&ctx(&real), &no_metrics, &InstanceContext::default()),
            0.0
        );
    }

    #[test]
    fn a_build_larger_than_the_unreserved_ram_is_penalized_before_the_first_heartbeat() {
        let cold = WorkerMetricsView {
            ram_total_mb: 16_000,
            cpu_usage_pct: None,
            ram_free_mb: None,
            ..Default::default()
        };
        let fits = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(8_000),
            samples: 5,
            ..Default::default()
        });
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(64_000),
            samples: 5,
            ..Default::default()
        });
        let sat = ResourceSaturationRule::default();
        assert_eq!(
            sat.score(&ctx(&fits), &worker_with(cold), &InstanceContext::default()),
            0.0
        );
        assert_eq!(
            sat.score(&ctx(&job), &worker_with(cold), &InstanceContext::default()),
            -PENALTY
        );

        let starved = WorkerMetricsView {
            ram_total_mb: 16_000,
            cpu_usage_pct: Some(10.0),
            ram_free_mb: Some(0),
            ..Default::default()
        };
        assert!(
            sat.score(
                &ctx(&job),
                &worker_with(starved),
                &InstanceContext::default()
            ) < 0.0
        );
    }

    fn just_reconnected(ram_reserved_mb: u64) -> WorkerContext<'static> {
        worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(0.2),
            ram_total_mb: 128_540,
            ram_free_mb: Some(118_426),
            ram_reserved_mb,
            ..Default::default()
        })
    }

    fn held(rule: &ResourceSaturationRule, job: &JobContext<'_>, w: &WorkerContext<'_>) -> bool {
        rule.veto(job, w, &InstanceContext::default())
    }

    #[test]
    fn ram_reserved_by_assigned_jobs_is_holding_a_burst_off_an_idle_looking_worker() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(24_000),
            samples: 4,
            ..Default::default()
        });

        let roomy = just_reconnected(77_000);
        assert_eq!(
            rule.score(&ctx(&job), &roomy, &InstanceContext::default()),
            0.0
        );
        assert!(!held(&rule, &ctx(&job), &roomy));

        let reserved = just_reconnected(110_000);
        assert_eq!(
            rule.score(&ctx(&job), &reserved, &InstanceContext::default()),
            -PENALTY
        );
        assert!(held(&rule, &ctx(&job), &reserved));
    }

    #[test]
    fn a_build_without_history_is_held_for_the_mean_peak_of_the_instance() {
        let rule = ResourceSaturationRule::default();
        let unmeasured = job_with_history(HistoryPrediction::default());
        let instance = InstanceContext {
            peak_ram_mb: crate::score::Windowed {
                w24h: Some(600.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let held =
            |job: &JobContext<'_>, reserved| rule.veto(job, &just_reconnected(reserved), &instance);

        assert!(!held(&ctx(&unmeasured), 127_000));
        assert!(held(&ctx(&unmeasured), 128_000));
        assert!(!held(&ctx(&builtin_job()), 128_000));

        let substitution = JobContext {
            substitute_outputs: Some(1),
            ..ctx(&unmeasured)
        };
        assert!(!held(&substitution, 128_000));
        assert!(!rule.veto(
            &ctx(&unmeasured),
            &just_reconnected(128_000),
            &InstanceContext::default()
        ));
    }

    #[test]
    fn reserved_ram_no_build_uses_yet_is_taken_off_the_measured_free_ram() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(30_000),
            samples: 4,
            ..Default::default()
        });
        let evaluations_using_40_gb = |ram_reserved_unused_mb| {
            worker_with(WorkerMetricsView {
                cpu_usage_pct: Some(0.2),
                ram_total_mb: 128_000,
                ram_free_mb: Some(88_000),
                ram_reserved_mb: 60_000,
                ram_reserved_unused_mb,
                ..Default::default()
            })
        };

        assert!(!held(&rule, &ctx(&job), &evaluations_using_40_gb(None)));
        assert!(!held(
            &rule,
            &ctx(&job),
            &evaluations_using_40_gb(Some(10_000))
        ));
        assert!(held(
            &rule,
            &ctx(&job),
            &evaluations_using_40_gb(Some(60_000))
        ));
    }

    #[test]
    fn ram_kept_for_a_waiting_build_is_holding_other_builds_off_the_worker() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(24_000),
            samples: 4,
            ..Default::default()
        });
        let keeping = |ram_reserved_for_waiting_build_mb| {
            worker_with(WorkerMetricsView {
                cpu_usage_pct: Some(0.2),
                ram_total_mb: 128_540,
                ram_free_mb: Some(118_426),
                ram_reserved_for_waiting_build_mb,
                ..Default::default()
            })
        };

        assert!(!held(&rule, &ctx(&job), &keeping(90_000)));
        assert!(held(&rule, &ctx(&job), &keeping(100_000)));
    }

    #[test]
    fn a_build_short_of_ram_on_a_worker_without_reservations_is_penalized_but_never_held() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(110_000),
            samples: 4,
            ..Default::default()
        });
        let idle = just_reconnected(0);

        assert_eq!(
            rule.score(&ctx(&job), &idle, &InstanceContext::default()),
            -PENALTY
        );
        assert!(!held(&rule, &ctx(&job), &idle));
    }

    #[test]
    fn a_build_larger_than_the_whole_worker_is_penalized_but_never_held() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(200_000),
            samples: 4,
            ..Default::default()
        });
        let busy = just_reconnected(50_000);

        assert_eq!(
            rule.score(&ctx(&job), &busy, &InstanceContext::default()),
            -PENALTY
        );
        assert!(!held(&rule, &ctx(&job), &busy));
    }

    #[test]
    fn ram_prediction_exceeding_free_penalizes_and_stacks_with_saturation() {
        let rule = ResourceSaturationRule::default();
        let job = job_with_history(HistoryPrediction {
            predicted_peak_ram_mb: Some(10_000),
            samples: 5,
            ..Default::default()
        });

        let tight = worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(10.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(8_000),
            ..Default::default()
        });
        assert_eq!(
            rule.score(&ctx(&job), &tight, &InstanceContext::default()),
            -PENALTY
        );

        let roomy = worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(10.0),
            ram_total_mb: 32_000,
            ram_free_mb: Some(12_000),
            ..Default::default()
        });
        assert_eq!(
            rule.score(&ctx(&job), &roomy, &InstanceContext::default()),
            0.0
        );

        let hot_and_tight = worker_with(WorkerMetricsView {
            cpu_usage_pct: Some(99.0),
            ram_total_mb: 16_000,
            ram_free_mb: Some(8_000),
            ..Default::default()
        });
        assert_eq!(
            rule.score(&ctx(&job), &hot_and_tight, &InstanceContext::default()),
            -2.0 * PENALTY
        );
    }
}
