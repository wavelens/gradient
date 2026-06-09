/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::InstanceContext;
use crate::rule::{JobContext, ScoreRule, WorkerContext};

/// Penalizes a job proportional to its owning org's share of currently-active
/// builds, so a quiet org's job is picked promptly even when a busy org floods
/// the queue (#111).
#[derive(Debug)]
pub struct FairShareRule {
    pub weight: f64,
}

impl Default for FairShareRule {
    fn default() -> Self {
        Self { weight: 500.0 }
    }
}

impl ScoreRule for FairShareRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> f64 {
        match job.org_work_share {
            Some(share) => -self.weight * share as f64,
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob};
    use crate::rules::builtin::WaitTimeRule;
    use gradient_core::types::ids::OrganizationId;
    use gradient_core::types::now;

    fn build_job() -> ScoredJob<'static> {
        ScoredJob::new_build(
            "j",
            OrganizationId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>, org_work_share: Option<f32>) -> JobContext<'a> {
        JobContext {
            job,
            missing_count: None,
            missing_nar_size: None,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            org_work_share,
            rescore_count: 0,
        }
    }

    fn worker() -> WorkerContext<'static> {
        WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: None }
    }

    #[test]
    fn busier_org_scores_more_negative() {
        let rule = FairShareRule::default();
        let job = build_job();
        let w = worker();
        let busy = rule.score(&ctx(&job, Some(0.99)), &w, &InstanceContext::default());
        let quiet = rule.score(&ctx(&job, Some(0.01)), &w, &InstanceContext::default());
        assert!(busy < quiet, "busy org must score lower: {busy} vs {quiet}");
    }

    #[test]
    fn zero_share_and_none_score_zero() {
        let rule = FairShareRule::default();
        let job = build_job();
        let w = worker();
        assert_eq!(rule.score(&ctx(&job, Some(0.0)), &w, &InstanceContext::default()), 0.0);
        assert_eq!(rule.score(&ctx(&job, None), &w, &InstanceContext::default()), 0.0);
    }

    // Among jobs with equal wait, the quieter org's job must score higher.
    #[test]
    fn fair_share_breaks_tie_at_equal_wait() {
        let fair = FairShareRule::default();
        let wait = WaitTimeRule::default();
        let job = build_job();
        let w = worker();
        let n = now();

        let quiet = JobContext { ready_at: n, queued_at: n, ..ctx(&job, Some(0.0)) };
        let busy = JobContext { ready_at: n, queued_at: n, ..ctx(&job, Some(1.0)) };

        let inst = InstanceContext::default();
        let quiet_total = fair.score(&quiet, &w, &inst) + wait.score(&quiet, &w, &inst);
        let busy_total = fair.score(&busy, &w, &inst) + wait.score(&busy, &w, &inst);
        assert!(quiet_total > busy_total, "at equal wait the quiet org wins: quiet={quiet_total} busy={busy_total}");
    }
}
