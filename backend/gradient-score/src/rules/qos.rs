/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::InstanceContext;
use crate::rule::{JobContext, ScoreRule, WorkerContext};

/// Quality of service: a job a user prioritized, directly or through the
/// evaluation or build that depends on it, goes ahead of everything else (#530).
#[derive(Debug)]
pub struct QosRule {
    pub prioritized: f64,
}

impl Default for QosRule {
    fn default() -> Self {
        Self {
            prioritized: crate::weights::QOS_PRIORITIZED,
        }
    }
}

impl ScoreRule for QosRule {
    fn name(&self) -> &'static str {
        "QosRule"
    }

    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        if job.prioritized {
            self.prioritized
        } else {
            0.0
        }
    }

    fn description(&self) -> &'static str {
        "Quality of service: lifts a job whose evaluation or dependent build a user prioritized above every unprioritized job."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, ScoredJob};
    use crate::rules::builtin::WaitTimeRule;
    use gradient_types::ids::ProjectId;
    use gradient_types::now;

    fn build_job() -> ScoredJob<'static> {
        ScoredJob::new_build(
            "j",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            None,
            HistoryPrediction::default(),
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>, prioritized: bool) -> JobContext<'a> {
        JobContext {
            job,
            missing_count: None,
            missing_nar_size: None,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized,
            rescore_count: 0,
            now: now(),
        }
    }

    fn worker() -> WorkerContext<'static> {
        WorkerContext {
            architectures: &[],
            system_features: &[],
            fetch: false,
            metrics: None,
        }
    }

    #[test]
    fn prioritized_scores_qos_and_unprioritized_scores_zero() {
        let rule = QosRule::default();
        let job = build_job();
        let inst = InstanceContext::default();
        assert_eq!(rule.score(&ctx(&job, true), &worker(), &inst), 5000.0);
        assert_eq!(rule.score(&ctx(&job, false), &worker(), &inst), 0.0);
    }

    #[test]
    fn fresh_prioritized_job_outranks_starving_unprioritized_job() {
        let qos = QosRule::default();
        let wait = WaitTimeRule::default();
        let job = build_job();
        let inst = InstanceContext::default();
        let fresh = ctx(&job, true);
        let starving = JobContext {
            queued_at: now() - chrono::Duration::days(1),
            ready_at: now() - chrono::Duration::days(1),
            ..ctx(&job, false)
        };
        let total =
            |c: &JobContext<'_>| qos.score(c, &worker(), &inst) + wait.score(c, &worker(), &inst);
        assert!(total(&fresh) > total(&starving));
    }
}
