/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

use crate::score::context::{HistoryPrediction, InstanceContext, WorkerMetricsView};
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};
use crate::score::weights;

const BYTES_PER_MIB: f64 = 1_048_576.0;
const BITS_PER_BYTE: f64 = 8.0;
const BITS_PER_MEGABIT: f64 = 1_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    MissingNarSize,
    MissingCount,
    BuildHistory,
    CoreScore,
    DownloadSpeed,
    UploadSpeed,
    StorageReadSpeed,
    StorageWriteSpeed,
    CompressionRatio,
    PerPathSecs,
    OutputNarSize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimeEstimate {
    pub download_secs: f64,
    pub path_secs: f64,
    pub build_secs: f64,
    pub oom_retry_secs: f64,
    pub upload_secs: f64,
    pub eval_secs: f64,
    pub nar_bytes: f64,
    pub paths: f64,
    pub output_nar_bytes: f64,
    pub oom_chance: f64,
    pub fallbacks: Vec<Fallback>,
}

impl TimeEstimate {
    pub fn total(&self) -> f64 {
        self.download_secs
            + self.path_secs
            + self.build_secs
            + self.oom_retry_secs
            + self.upload_secs
            + self.eval_secs
    }
}

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

fn known<T>(value: Option<T>, fallback: Fallback, fallbacks: &mut Vec<Fallback>) -> Option<T> {
    if value.is_none() {
        fallbacks.push(fallback);
    }
    value
}

fn stored_per_nar_byte(instance: &InstanceContext, fallbacks: &mut Vec<Fallback>) -> f64 {
    known(
        instance.compression_ratio.filter(|r| *r > 0.0),
        Fallback::CompressionRatio,
        fallbacks,
    )
    .unwrap_or(weights::STORED_PER_NAR_BYTE_FALLBACK)
}

fn speed(
    own: Option<f32>,
    fleet_mean: Option<f64>,
    fallback: Fallback,
    fallbacks: &mut Vec<Fallback>,
) -> Option<f64> {
    known(own.map(f64::from), fallback, fallbacks).or(fleet_mean)
}

fn missing_nar_bytes(
    job: &JobContext<'_>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> f64 {
    known(
        job.missing_nar_size.map(|b| b as f64),
        Fallback::MissingNarSize,
        fallbacks,
    )
    .or_else(|| instance.nar_size_mb.w1h.map(|mb| mb * BYTES_PER_MIB))
    .unwrap_or(0.0)
}

fn missing_paths(
    job: &JobContext<'_>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> f64 {
    known(
        job.missing_count.map(f64::from),
        Fallback::MissingCount,
        fallbacks,
    )
    .or(instance.missing_paths.w1h)
    .unwrap_or(0.0)
}

fn output_nar_bytes(history: &HistoryPrediction, fallbacks: &mut Vec<Fallback>) -> f64 {
    known(history.output_nar_size, Fallback::OutputNarSize, fallbacks).unwrap_or(0) as f64
}

fn download_secs(
    nar_bytes: f64,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> f64 {
    transfer_secs(Transfer {
        nar_bytes,
        worker_mbps: speed(
            worker.and_then(|m| m.download_speed_mbps),
            instance.download_speed_mean_mbps,
            Fallback::DownloadSpeed,
            fallbacks,
        ),
        storage_mbps: known(
            instance.storage_read_mbps,
            Fallback::StorageReadSpeed,
            fallbacks,
        )
        .unwrap_or(weights::STORAGE_READ_FALLBACK_MBPS),
        stored_per_nar_byte: stored_per_nar_byte(instance, fallbacks),
        in_flight: instance.downloads_in_flight,
    })
}

fn path_secs(paths: f64, instance: &InstanceContext, fallbacks: &mut Vec<Fallback>) -> f64 {
    paths
        * known(instance.per_path_secs, Fallback::PerPathSecs, fallbacks)
            .unwrap_or(weights::PER_PATH_FALLBACK_SECS)
}

fn upload_secs(
    output_nar_bytes: f64,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> f64 {
    transfer_secs(Transfer {
        nar_bytes: output_nar_bytes,
        worker_mbps: speed(
            worker.and_then(|m| m.upload_speed_mbps),
            instance.upload_speed_mean_mbps,
            Fallback::UploadSpeed,
            fallbacks,
        ),
        storage_mbps: known(
            instance.storage_write_mbps,
            Fallback::StorageWriteSpeed,
            fallbacks,
        )
        .unwrap_or(weights::STORAGE_WRITE_FALLBACK_MBPS),
        stored_per_nar_byte: stored_per_nar_byte(instance, fallbacks),
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

fn run_secs(
    history: &HistoryPrediction,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallback_ms: Option<f64>,
    fallbacks: &mut Vec<Fallback>,
) -> f64 {
    let fleet = instance.cpu_core_score_mean;
    if history.from_fleet_mean {
        fallbacks.push(Fallback::BuildHistory);
    }
    let (uncontended_ms, built_on) = match history.uncontended_build_time_ms {
        Some(ms) => (
            ms as f64,
            known(history.build_core_score, Fallback::CoreScore, fallbacks)
                .map(f64::from)
                .or(fleet),
        ),
        None => {
            fallbacks.push(Fallback::BuildHistory);
            match fallback_ms {
                Some(ms) => {
                    fallbacks.push(Fallback::CoreScore);
                    (ms, fleet)
                }
                None => return 0.0,
            }
        }
    };
    let runs_on = known(
        worker.map(|m| m.cpu_core_score).filter(|score| *score > 0),
        Fallback::CoreScore,
        fallbacks,
    )
    .map(f64::from)
    .or(fleet);
    let running = worker.map_or(0, |m| m.running_builds);

    uncontended_ms / 1000.0 * core_ratio(built_on, runs_on) * contention_factor(running)
}

fn build_secs(
    history: &HistoryPrediction,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> f64 {
    let window = instance.build_time_ms.w1h.or(instance.build_time_ms.w24h);
    run_secs(history, worker, instance, window, fallbacks)
}

fn oom_chance(history: &HistoryPrediction, worker: Option<&WorkerMetricsView>) -> f64 {
    let overshoot = match (
        history.predicted_peak_ram_mb,
        worker.and_then(|m| m.ram_free_mb),
    ) {
        (Some(peak), Some(free)) if free > 0 && peak > free => {
            ((peak - free) as f64 / free as f64).min(1.0)
        }
        _ => 0.0,
    };

    (f64::from(history.oom_rate) + overshoot).min(1.0)
}

fn eval_estimate(
    history: &HistoryPrediction,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> TimeEstimate {
    let eval_secs = run_secs(history, worker, instance, None, fallbacks);
    let oom_chance = oom_chance(history, worker);
    TimeEstimate {
        eval_secs,
        oom_chance,
        oom_retry_secs: oom_chance * eval_secs,
        ..Default::default()
    }
}

fn upload_estimate(
    history: &HistoryPrediction,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> TimeEstimate {
    let output_nar_bytes = output_nar_bytes(history, fallbacks);
    TimeEstimate {
        upload_secs: upload_secs(output_nar_bytes, worker, instance, fallbacks),
        output_nar_bytes,
        ..Default::default()
    }
}

fn build_estimate(
    job: &JobContext<'_>,
    worker: Option<&WorkerMetricsView>,
    instance: &InstanceContext,
    fallbacks: &mut Vec<Fallback>,
) -> TimeEstimate {
    let history = job.build_history();
    let nar_bytes = missing_nar_bytes(job, instance, fallbacks);
    let paths = missing_paths(job, instance, fallbacks);
    let build_secs = build_secs(&history, worker, instance, fallbacks);
    let oom_chance = oom_chance(&history, worker);
    let output_nar_bytes = output_nar_bytes(&history, fallbacks);
    TimeEstimate {
        download_secs: download_secs(nar_bytes, worker, instance, fallbacks),
        path_secs: path_secs(paths, instance, fallbacks),
        build_secs,
        oom_retry_secs: oom_chance * build_secs,
        upload_secs: upload_secs(output_nar_bytes, worker, instance, fallbacks),
        nar_bytes,
        paths,
        output_nar_bytes,
        oom_chance,
        ..Default::default()
    }
}

pub fn estimate(
    job: &JobContext<'_>,
    worker: &WorkerContext<'_>,
    instance: &InstanceContext,
) -> TimeEstimate {
    let metrics = worker.metrics.as_ref();
    let mut fallbacks = Vec::new();
    let mut estimate = if job.job.build().is_none() {
        eval_estimate(&job.job.history(), metrics, instance, &mut fallbacks)
    } else if job.outputs_present {
        upload_estimate(&job.job.history(), metrics, instance, &mut fallbacks)
    } else {
        build_estimate(job, metrics, instance, &mut fallbacks)
    };
    fallbacks.sort_unstable();
    fallbacks.dedup();
    estimate.fallbacks = fallbacks;
    estimate
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
        let estimate = estimate(job, worker, instance).total().min(self.cap_secs);
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

    fn secs(
        history: &HistoryPrediction,
        worker: &WorkerMetricsView,
        inst: &InstanceContext,
    ) -> f64 {
        build_secs(history, Some(worker), inst, &mut Vec::new())
    }

    #[test]
    fn a_build_takes_longer_on_a_slower_core_and_beside_running_builds() {
        let history = built(100_000, Some(3_000));
        let inst = InstanceContext::default();

        assert_eq!(secs(&history, &on(3_000, 0), &inst), 100.0);
        assert_eq!(secs(&history, &on(1_500, 0), &inst), 200.0);
        assert!((secs(&history, &on(1_500, 5), &inst) - 240.0).abs() < 1e-9);
    }

    #[test]
    fn the_core_score_ratio_is_bounded() {
        let history = built(100_000, Some(3_000));
        let inst = InstanceContext::default();

        assert_eq!(secs(&history, &on(1, 0), &inst), 200.0);
        assert_eq!(secs(&history, &on(1_000_000, 0), &inst), 50.0);
    }

    #[test]
    fn an_unknown_core_score_is_the_fleet_mean() {
        let inst = InstanceContext {
            cpu_core_score_mean: Some(3_000.0),
            ..Default::default()
        };

        assert_eq!(secs(&built(100_000, None), &on(1_500, 0), &inst), 200.0);
        assert_eq!(secs(&built(100_000, Some(1_500)), &on(0, 0), &inst), 50.0);
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
            secs(&HistoryPrediction::default(), &on(2_000, 0), &inst),
            60.0
        );
        assert_eq!(
            build_secs(
                &HistoryPrediction::default(),
                None,
                &InstanceContext::default(),
                &mut Vec::new()
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
            ..Default::default()
        }
    }

    fn worker(metrics: WorkerMetricsView) -> WorkerContext<'static> {
        WorkerContext {
            metrics: Some(metrics),
            ..Default::default()
        }
    }

    #[test]
    fn every_missing_path_costs_the_learned_overhead() {
        let job = build_job(HistoryPrediction::default());
        let inst = InstanceContext {
            per_path_secs: Some(0.5),
            missing_paths: Windowed {
                w1h: Some(8.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let scored = JobContext {
            missing_count: Some(6),
            ..ctx(&job, Some(0), false)
        };
        let unscored = JobContext {
            missing_count: None,
            ..ctx(&job, None, false)
        };
        let f = &mut Vec::new();

        assert_eq!(path_secs(missing_paths(&scored, &inst, f), &inst, f), 3.0);
        assert_eq!(path_secs(missing_paths(&unscored, &inst, f), &inst, f), 4.0);
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
            "a worker holding the outputs builds nothing"
        );
    }

    #[test]
    fn a_build_that_may_run_out_of_memory_expects_to_lose_its_time_again() {
        let history = HistoryPrediction {
            predicted_peak_ram_mb: Some(3_000),
            oom_rate: 0.25,
            ..built(100_000, None)
        };
        let with_free = |ram_free_mb| WorkerMetricsView {
            ram_free_mb: Some(ram_free_mb),
            ..Default::default()
        };

        assert_eq!(oom_chance(&history, Some(&with_free(8_000))), 0.25);
        assert_eq!(oom_chance(&history, Some(&with_free(2_000))), 0.75);
        assert_eq!(oom_chance(&history, Some(&with_free(1_000))), 1.0);
    }

    #[test]
    fn a_worker_holding_the_outputs_only_uploads_them() {
        let job = build_job(HistoryPrediction {
            output_nar_size: Some(GIGABYTE as u64),
            ..built(100_000, None)
        });
        let w = worker(WorkerMetricsView {
            upload_speed_mbps: Some(400.0),
            ..Default::default()
        });
        let inst = InstanceContext {
            storage_write_mbps: Some(1_000_000.0),
            ..Default::default()
        };

        let e = estimate(&ctx(&job, Some(0), true), &w, &inst);
        assert!((e.upload_secs - 20.0).abs() < 1e-9);
        assert_eq!(e.total(), e.upload_secs);
        assert_eq!(e.build_secs, 0.0);
    }

    #[test]
    fn the_estimate_records_download_build_and_upload_apart() {
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

        let e = estimate(&ctx(&job, Some(GIGABYTE as u64), false), &w, &inst);
        assert!((e.download_secs - 10.0).abs() < 1e-9, "{e:?}");
        assert!((e.build_secs - 30.0).abs() < 1e-9, "{e:?}");
        assert!((e.upload_secs - 20.0).abs() < 1e-9, "{e:?}");
        assert!((e.total() - 60.0).abs() < 1e-9, "{e:?}");
        assert_eq!(e.nar_bytes, GIGABYTE);
        assert_eq!(e.output_nar_bytes, GIGABYTE);
    }

    #[test]
    fn the_estimate_names_each_input_it_had_to_guess() {
        let job = build_job(HistoryPrediction::default());
        let c = JobContext {
            missing_count: None,
            ..ctx(&job, None, false)
        };
        let w = WorkerContext::default();
        let inst = InstanceContext {
            build_time_ms: Windowed {
                w24h: Some(60_000.0),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            estimate(&c, &w, &inst).fallbacks,
            vec![
                Fallback::MissingNarSize,
                Fallback::MissingCount,
                Fallback::BuildHistory,
                Fallback::CoreScore,
                Fallback::DownloadSpeed,
                Fallback::UploadSpeed,
                Fallback::StorageReadSpeed,
                Fallback::StorageWriteSpeed,
                Fallback::CompressionRatio,
                Fallback::PerPathSecs,
                Fallback::OutputNarSize,
            ]
        );
    }

    #[test]
    fn a_fully_measured_build_guesses_nothing() {
        let job = build_job(HistoryPrediction {
            output_nar_size: Some(1),
            ..built(30_000, Some(2_000))
        });
        let c = JobContext {
            missing_count: Some(3),
            ..ctx(&job, Some(1), false)
        };
        let w = worker(WorkerMetricsView {
            cpu_core_score: 2_000,
            download_speed_mbps: Some(100.0),
            upload_speed_mbps: Some(100.0),
            ..Default::default()
        });
        let inst = InstanceContext {
            storage_read_mbps: Some(1_000.0),
            storage_write_mbps: Some(1_000.0),
            compression_ratio: Some(0.5),
            per_path_secs: Some(0.1),
            ..Default::default()
        };

        assert_eq!(estimate(&c, &w, &inst).fallbacks, Vec::new());
    }

    #[test]
    fn an_evaluation_timed_by_the_fleet_mean_says_so() {
        let fleet = ScoredJob::new_eval(
            "e",
            ProjectId::now_v7(),
            true,
            HistoryPrediction {
                from_fleet_mean: true,
                ..built(100_000, None)
            },
        );
        let own = ScoredJob::new_eval("e", ProjectId::now_v7(), true, built(100_000, None));
        let w = worker(on(1_000, 0));
        let inst = InstanceContext::default();

        assert!(
            estimate(&ctx(&fleet, None, false), &w, &inst)
                .fallbacks
                .contains(&Fallback::BuildHistory)
        );
        assert!(
            !estimate(&ctx(&own, None, false), &w, &inst)
                .fallbacks
                .contains(&Fallback::BuildHistory)
        );
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
    fn an_evaluation_is_estimated_by_its_run_time_alone() {
        let rule = EstimatedTimeRule::default();
        let eval = ScoredJob::new_eval("e", ProjectId::now_v7(), true, built(100_000, None));
        let unknown =
            ScoredJob::new_eval("e", ProjectId::now_v7(), true, HistoryPrediction::default());
        let inst = InstanceContext {
            per_path_secs: Some(1.0),
            build_time_ms: Windowed {
                w1h: Some(999_000.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let w = worker(on(1_000, 0));

        assert_eq!(
            rule.score(&ctx(&eval, Some(10), false), &w, &inst),
            rule.points_per_sec * (rule.cap_secs - 100.0)
        );
        assert_eq!(
            rule.score(&ctx(&unknown, None, false), &w, &inst),
            rule.points_per_sec * rule.cap_secs,
            "an evaluation never takes the build time of the instance"
        );
    }
}
