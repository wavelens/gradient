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
        match job.org_share {
            Some(share) => -self.weight * share as f64,
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, JobKindView, LazyProviders, ScoredJob};
    use crate::rules::builtin::WaitTimeRule;
    use gradient_core::types::ids::OrganizationId;
    use gradient_core::types::now;

    fn build_job() -> ScoredJob<'static> {
        ScoredJob::new(
            "j",
            OrganizationId::now_v7(),
            JobKindView::Build,
            "x86_64-linux",
            false,
            false,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>, org_share: Option<f32>) -> JobContext<'a> {
        JobContext {
            job,
            missing_count: None,
            missing_nar_size: None,
            dependency_count: 0,
            queued_at: now(),
            org_share,
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

    // #111: a quiet org (share ~0) must overcome a busy org (share ~1) even when
    // the busy job has maxed out WaitTimeRule's wait bonus. The fair-share weight
    // (500) must exceed WaitTimeRule's plateau (max_wait_secs*bonus_per_second).
    #[test]
    fn fair_share_overrides_wait_gradient() {
        let fair = FairShareRule::default();
        let wait = WaitTimeRule::default();
        let job = build_job();
        let w = worker();

        let quiet = ctx(&job, Some(0.0));
        let busy = JobContext {
            queued_at: now() - chrono::Duration::seconds(10_000),
            ..ctx(&job, Some(1.0))
        };

        let quiet_total = fair.score(&quiet, &w, &InstanceContext::default()) + wait.score(&quiet, &w, &InstanceContext::default());
        let busy_total = fair.score(&busy, &w, &InstanceContext::default()) + wait.score(&busy, &w, &InstanceContext::default());
        assert!(
            quiet_total > busy_total,
            "quiet org must win despite busy org's wait bonus: quiet={quiet_total} busy={busy_total}"
        );
    }
}
