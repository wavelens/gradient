/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::messages::SMALL_UPLOAD_BYTES;

use crate::score::context::InstanceContext;
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};

const BYTES_PER_MIB: f64 = 1_048_576.0;

fn slots_full(in_flight: u32, slots: u32) -> bool {
    slots > 0 && in_flight >= slots
}

fn holds_download(job: &JobContext<'_>, instance: &InstanceContext) -> bool {
    let mean_bytes = instance.nar_size_mb.w1h.unwrap_or(0.0) * BYTES_PER_MIB;
    !job.outputs_present
        && slots_full(instance.downloads_in_flight, instance.download_slots)
        && job
            .missing_nar_size
            .is_some_and(|bytes| bytes as f64 > mean_bytes)
}

fn holds_upload(job: &JobContext<'_>, instance: &InstanceContext) -> bool {
    slots_full(instance.uploads_in_flight, instance.upload_slots)
        && job
            .job
            .history()
            .output_nar_size
            .is_some_and(|bytes| bytes > SMALL_UPLOAD_BYTES)
}

#[derive(Debug)]
pub struct TransferLimitRule {
    pub hold: f64,
}

impl Default for TransferLimitRule {
    fn default() -> Self {
        Self {
            hold: crate::score::weights::TRANSFER_LIMIT_HOLD,
        }
    }
}

impl ScoreRule for TransferLimitRule {
    fn name(&self) -> &'static str {
        "TransferLimitRule"
    }

    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        if job.job.build().is_none() {
            return 0.0;
        }

        if holds_download(job, instance) || holds_upload(job, instance) {
            -self.hold
        } else {
            0.0
        }
    }

    fn description(&self) -> &'static str {
        "Holds a build with a larger than average download, or an output over 1 MiB, below the assignment floor while the server's download or upload slots are full. Jobs with local inputs take the slot, and the wait bonus releases a held job."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::context::{HistoryPrediction, ScoredJob, Windowed};
    use gradient_types::ids::ProjectId;

    fn build(output_nar_size: Option<u64>) -> ScoredJob<'static> {
        ScoredJob::new_build(
            "j",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            None,
            HistoryPrediction {
                output_nar_size,
                ..Default::default()
            },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>, missing_nar_size: u64) -> JobContext<'a> {
        JobContext {
            job,
            missing_count: Some(0),
            missing_nar_size: Some(missing_nar_size),
            ..Default::default()
        }
    }

    fn worker() -> WorkerContext<'static> {
        WorkerContext::default()
    }

    fn downloads(in_flight: u32) -> InstanceContext {
        InstanceContext {
            downloads_in_flight: in_flight,
            download_slots: 16,
            nar_size_mb: Windowed {
                w1h: Some(100.0),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_large_download_waits_while_the_download_slots_are_full() {
        let rule = TransferLimitRule::default();
        let job = build(None);
        let large = 500 * 1_048_576;

        assert_eq!(
            rule.score(&ctx(&job, large), &worker(), &downloads(16)),
            -rule.hold
        );
        assert_eq!(
            rule.score(&ctx(&job, large), &worker(), &downloads(15)),
            0.0
        );
        assert_eq!(
            rule.score(&ctx(&job, 1_048_576), &worker(), &downloads(16)),
            0.0,
            "a job with local inputs takes the slot"
        );
    }

    #[test]
    fn a_large_output_waits_while_the_upload_slots_are_full() {
        let rule = TransferLimitRule::default();
        let full = InstanceContext {
            uploads_in_flight: 8,
            upload_slots: 8,
            ..Default::default()
        };
        let large = build(Some(SMALL_UPLOAD_BYTES + 1));
        let small = build(Some(SMALL_UPLOAD_BYTES));

        assert_eq!(rule.score(&ctx(&large, 0), &worker(), &full), -rule.hold);
        assert_eq!(rule.score(&ctx(&small, 0), &worker(), &full), 0.0);
    }
}
