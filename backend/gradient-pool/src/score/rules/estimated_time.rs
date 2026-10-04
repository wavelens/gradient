/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::score::context::{HistoryPrediction, InstanceContext, WorkerMetricsView};
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};
use crate::score::weights;

const BYTES_PER_MIB: f64 = 1_048_576.0;
const BITS_PER_BYTE: f64 = 8.0;
const BITS_PER_MEGABIT: f64 = 1_000_000.0;

pub fn contention_factor(other_builds: u32) -> f64 {
    1.0 + weights::BUILD_CONTENTION_PER_BUILD * f64::from(other_builds)
}

pub struct Transfer {
    pub nar_bytes: f64,
    pub worker_mbps: Option<f64>,
    pub storage_mbps: f64,
    pub stored_per_nar_byte: f64,
    pub in_flight: u32,
}

pub fn transfer_secs(t: Transfer) -> f64 {
    let storage_share =
        t.storage_mbps / t.stored_per_nar_byte / f64::from(t.in_flight.saturating_add(1));
    let mbps = match t.worker_mbps.filter(|w| *w > 0.0) {
        Some(worker) => worker.min(storage_share),
        None => storage_share,
    };
    if t.nar_bytes <= 0.0 || mbps <= 0.0 {
        return 0.0;
    }

    t.nar_bytes * BITS_PER_BYTE / BITS_PER_MEGABIT / mbps
}

fn stored_per_nar_byte(instance: &InstanceContext) -> f64 {
    instance
        .compression_ratio
        .filter(|r| *r > 0.0)
        .unwrap_or(weights::STORED_PER_NAR_BYTE_FALLBACK)
}

fn speed(own: Option<f32>, fleet_mean: Option<f64>) -> Option<f64> {
    own.map(f64::from).or(fleet_mean)
}

pub fn download_secs(
    job: &JobContext<'_>,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
) -> f64 {
    let nar_bytes = job
        .missing_nar_size
        .map(|b| b as f64)
        .or_else(|| instance.nar_size_mb.w1h.map(|mb| mb * BYTES_PER_MIB))
        .unwrap_or(0.0);

    transfer_secs(Transfer {
        nar_bytes,
        worker_mbps: speed(
            worker.and_then(|m| m.download_speed_mbps),
            instance.download_speed_mean_mbps,
        ),
        storage_mbps: instance
            .storage_read_mbps
            .unwrap_or(weights::STORAGE_READ_FALLBACK_MBPS),
        stored_per_nar_byte: stored_per_nar_byte(instance),
        in_flight: instance.downloads_in_flight,
    })
}

pub fn upload_secs(
    history: &HistoryPrediction,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
) -> f64 {
    transfer_secs(Transfer {
        nar_bytes: history.output_nar_size.unwrap_or(0) as f64,
        worker_mbps: speed(
            worker.and_then(|m| m.upload_speed_mbps),
            instance.upload_speed_mean_mbps,
        ),
        storage_mbps: instance
            .storage_write_mbps
            .unwrap_or(weights::STORAGE_WRITE_FALLBACK_MBPS),
        stored_per_nar_byte: stored_per_nar_byte(instance),
        in_flight: instance.uploads_in_flight,
    })
}

fn core_ratio(built_on: Option<f64>, runs_on: Option<f64>) -> f64 {
    match (built_on, runs_on) {
        (Some(built), Some(runs)) if built > 0.0 && runs > 0.0 => {
            (built / runs).clamp(weights::CORE_SCORE_RATIO_MIN, weights::CORE_SCORE_RATIO_MAX)
        }
        _ => 1.0,
    }
}

pub fn build_secs(
    history: &HistoryPrediction,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
) -> f64 {
    let fleet = instance.cpu_core_score_mean;
    let (uncontended_ms, built_on) = match history.uncontended_build_time_ms {
        Some(ms) => (ms as f64, history.build_core_score.map(f64::from).or(fleet)),
        None => match instance.build_time_ms.w1h.or(instance.build_time_ms.w24h) {
            Some(ms) => (ms, fleet),
            None => return 0.0,
        },
    };
    let runs_on = worker
        .map(|m| m.cpu_core_score)
        .filter(|score| *score > 0)
        .map(f64::from)
        .or(fleet);
    let running = worker.map_or(0, |m| m.running_builds);

    uncontended_ms / 1000.0 * core_ratio(built_on, runs_on) * contention_factor(running)
}

pub fn estimated_secs(
    job: &JobContext<'_>,
    worker: &WorkerContext<'_>,
    instance: &InstanceContext,
) -> f64 {
    if job.outputs_present {
        return 0.0;
    }

    let history = job.build_history();
    let metrics = worker.metrics.as_ref();
    download_secs(job, metrics, instance)
        + build_secs(&history, metrics, instance)
        + upload_secs(&history, metrics, instance)
}

#[derive(Debug)]
pub struct EstimatedTimeRule {
    pub points_per_sec: f64,
    pub cap_secs: f64,
}

impl Default for EstimatedTimeRule {
    fn default() -> Self {
        Self {
            points_per_sec: weights::ESTIMATED_TIME_POINTS_PER_SEC,
            cap_secs: weights::ESTIMATED_TIME_CAP_SECS,
        }
    }
}

impl ScoreRule for EstimatedTimeRule {
    fn name(&self) -> &'static str {
        "EstimatedTimeRule"
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

        let estimate = estimated_secs(job, worker, instance).min(self.cap_secs);
        self.points_per_sec * (self.cap_secs - estimate)
    }

    fn description(&self) -> &'static str {
        "Ranks builds by the seconds they are expected to take on this worker: downloading the missing inputs, building, and uploading the outputs. Each second costs one point, up to a cap."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::context::{ScoredJob, Windowed};
    use gradient_types::ids::ProjectId;

    const GIGABYTE: f64 = 1_000_000_000.0;

    fn transfer(worker_mbps: Option<f64>, in_flight: u32, stored_per_nar_byte: f64) -> f64 {
        transfer_secs(Transfer {
            nar_bytes: GIGABYTE,
            worker_mbps,
            storage_mbps: 1_200.0,
            stored_per_nar_byte,
            in_flight,
        })
    }

    #[test]
    fn a_transfer_runs_at_the_slower_of_the_worker_and_its_storage_share() {
        assert_eq!(transfer(Some(100.0), 0, 1.0), 80.0);
        assert_eq!(transfer(Some(100.0), 23, 1.0), 160.0);
        assert_eq!(transfer(None, 23, 1.0), 160.0);
    }

    #[test]
    fn compressed_storage_moves_more_nar_bytes_per_second() {
        assert_eq!(transfer(None, 23, 0.5), 80.0);
    }

    #[test]
    fn nothing_to_move_takes_no_time() {
        let none = Transfer {
            nar_bytes: 0.0,
            worker_mbps: Some(1.0),
            storage_mbps: 1.0,
            stored_per_nar_byte: 1.0,
            in_flight: 0,
        };
        assert_eq!(transfer_secs(none), 0.0);
    }

    fn built(ms: u64, core_score: Option<u32>) -> HistoryPrediction {
        HistoryPrediction {
            build_time_ms: Some(ms),
            uncontended_build_time_ms: Some(ms),
            build_core_score: core_score,
            samples: 3,
            ..Default::default()
        }
    }

    fn on(cpu_core_score: u32, running_builds: u32) -> WorkerMetricsView {
        WorkerMetricsView {
            cpu_core_score,
            running_builds,
            ..Default::default()
        }
    }

    #[test]
    fn a_build_takes_longer_on_a_slower_core_and_beside_running_builds() {
        let history = built(100_000, Some(3_000));
        let inst = InstanceContext::default();

        assert_eq!(build_secs(&history, Some(&on(3_000, 0)), &inst), 100.0);
        assert_eq!(build_secs(&history, Some(&on(1_500, 0)), &inst), 200.0);
        assert!((build_secs(&history, Some(&on(1_500, 5)), &inst) - 240.0).abs() < 1e-9);
    }

    #[test]
    fn the_core_score_ratio_is_bounded() {
        let history = built(100_000, Some(3_000));
        let inst = InstanceContext::default();

        assert_eq!(build_secs(&history, Some(&on(1, 0)), &inst), 200.0);
        assert_eq!(build_secs(&history, Some(&on(1_000_000, 0)), &inst), 50.0);
    }

    #[test]
    fn an_unknown_core_score_is_the_fleet_mean() {
        let inst = InstanceContext {
            cpu_core_score_mean: Some(3_000.0),
            ..Default::default()
        };

        assert_eq!(
            build_secs(&built(100_000, None), Some(&on(1_500, 0)), &inst),
            200.0
        );
        assert_eq!(
            build_secs(&built(100_000, Some(1_500)), Some(&on(0, 0)), &inst),
            50.0
        );
    }

    #[test]
    fn without_history_the_instance_build_time_is_the_estimate() {
        let inst = InstanceContext {
            build_time_ms: Windowed {
                w24h: Some(60_000.0),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            build_secs(&HistoryPrediction::default(), Some(&on(2_000, 0)), &inst),
            60.0
        );
        assert_eq!(
            build_secs(
                &HistoryPrediction::default(),
                None,
                &InstanceContext::default()
            ),
            0.0
        );
    }

    fn build_job(history: HistoryPrediction) -> ScoredJob<'static> {
        ScoredJob::new_build(
            "j",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            None,
            history,
        )
    }

    fn ctx<'a>(
        job: &'a ScoredJob<'a>,
        missing_nar_size: Option<u64>,
        outputs_present: bool,
    ) -> JobContext<'a> {
        JobContext {
            job,
            missing_count: Some(0),
            missing_nar_size,
            outputs_present,
            dependency_count: 0,
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: gradient_types::now(),
        }
    }

    fn worker(metrics: WorkerMetricsView) -> WorkerContext<'static> {
        WorkerContext {
            architectures: &[],
            system_features: &[],
            fetch: false,
            metrics: Some(metrics),
        }
    }

    #[test]
    fn the_score_is_the_capped_time_left_unspent() {
        let rule = EstimatedTimeRule::default();
        let inst = InstanceContext::default();
        let short = build_job(built(100_000, None));
        let endless = build_job(built(100_000_000, None));
        let w = worker(on(1_000, 0));

        assert_eq!(
            rule.score(&ctx(&short, Some(0), false), &w, &inst),
            rule.points_per_sec * (rule.cap_secs - 100.0)
        );
        assert_eq!(rule.score(&ctx(&endless, Some(0), false), &w, &inst), 0.0);
        assert_eq!(
            rule.score(&ctx(&endless, Some(0), true), &w, &inst),
            rule.points_per_sec * rule.cap_secs,
            "a worker holding the outputs only uploads them"
        );
    }

    #[test]
    fn the_estimate_sums_download_build_and_upload() {
        let history = HistoryPrediction {
            output_nar_size: Some(GIGABYTE as u64),
            ..built(30_000, None)
        };
        let job = build_job(history);
        let inst = InstanceContext {
            storage_read_mbps: Some(1_000_000.0),
            storage_write_mbps: Some(1_000_000.0),
            ..Default::default()
        };
        let w = worker(WorkerMetricsView {
            download_speed_mbps: Some(800.0),
            upload_speed_mbps: Some(400.0),
            ..Default::default()
        });

        let estimate = estimated_secs(&ctx(&job, Some(GIGABYTE as u64), false), &w, &inst);
        assert!((estimate - (10.0 + 30.0 + 20.0)).abs() < 1e-9, "{estimate}");
    }

    #[test]
    fn a_large_download_goes_to_the_faster_downloading_worker() {
        let rule = EstimatedTimeRule::default();
        let job = build_job(HistoryPrediction::default());
        let c = ctx(&job, Some(4 * GIGABYTE as u64), false);
        let downloading = |download_speed_mbps| {
            worker(WorkerMetricsView {
                download_speed_mbps: Some(download_speed_mbps),
                ..Default::default()
            })
        };
        let inst = InstanceContext::default();

        assert!(
            rule.score(&c, &downloading(1_000.0), &inst)
                > rule.score(&c, &downloading(100.0), &inst)
        );
    }

    #[test]
    fn an_evaluation_is_not_estimated() {
        let rule = EstimatedTimeRule::default();
        let eval = ScoredJob::new_eval("e", ProjectId::now_v7(), true, built(100_000, None));

        assert_eq!(
            rule.score(
                &ctx(&eval, None, false),
                &worker(on(1, 0)),
                &InstanceContext::default()
            ),
            0.0
        );
    }
}
