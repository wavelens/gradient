/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::score::context::InstanceContext;
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug)]
pub struct QosRule {
    pub prioritized: f64,
    pub build_request: f64,
    pub ifd: f64,
}

impl Default for QosRule {
    fn default() -> Self {
        Self {
            prioritized: crate::score::weights::QOS_PRIORITIZED,
            build_request: crate::score::weights::QOS_BUILD_REQUEST,
            ifd: crate::score::weights::QOS_IFD,
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
        let lift = |on: bool, weight: f64| if on { weight } else { 0.0 };
        lift(job.prioritized, self.prioritized)
            + lift(job.build_request, self.build_request)
            + lift(job.ifd, self.ifd)
    }

    fn description(&self) -> &'static str {
        "Quality of service: lifts a job whose evaluation or dependent build a user prioritized above every unprioritized job, and a job of a build request above other jobs, and a build an evaluation imports, or a dependency of it, above other builds."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::context::{HistoryPrediction, ScoredJob};
    use crate::score::rules::builtin::WaitTimeRule;
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
            outputs_present: false,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized,
            build_request: false,
            ifd: false,
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

    #[test]
    fn a_build_request_ranks_between_a_prioritized_and_a_plain_job() {
        let qos = QosRule::default();
        let job = build_job();
        let inst = InstanceContext::default();
        let score = |prioritized, build_request| {
            let c = JobContext {
                build_request,
                ..ctx(&job, prioritized)
            };
            qos.score(&c, &worker(), &inst)
        };
        assert!(score(true, false) > score(false, true));
        assert!(score(false, true) > score(false, false));
        assert!(score(true, true) > score(true, false));
    }

    #[test]
    fn an_ifd_job_gains_the_lift_and_stacks_with_prioritized() {
        let qos = QosRule::default();
        let job = build_job();
        let inst = InstanceContext::default();
        let score = |prioritized, ifd| {
            let c = JobContext {
                ifd,
                ..ctx(&job, prioritized)
            };
            qos.score(&c, &worker(), &inst)
        };
        assert_eq!(
            score(false, true) - score(false, false),
            QosRule::default().ifd
        );
        assert_eq!(
            score(true, true),
            QosRule::default().prioritized + QosRule::default().ifd
        );
        assert!(score(true, false) > score(false, true));
    }
}
