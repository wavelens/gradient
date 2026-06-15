/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::InstanceContext;
use crate::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug)]
pub struct PreferLocalBuildRule {
    pub local_bonus: f64,
    pub miss_penalty: f64,
}

impl Default for PreferLocalBuildRule {
    fn default() -> Self {
        Self { local_bonus: 150.0, miss_penalty: 20.0 }
    }
}

impl ScoreRule for PreferLocalBuildRule {
    fn score(
        &self,
        job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        let Some(b) = job.job.build() else { return 0.0 };
        if !b.prefer_local_build {
            return 0.0;
        }

        let knee = 2.0 * instance.missing_paths.w1h;
        let slope = if knee > 0.0 { self.local_bonus / knee } else { self.miss_penalty };
        match job.missing_count {
            Some(0) => self.local_bonus,
            Some(n) => (self.local_bonus - n as f64 * slope).max(0.0),
            None => 0.0,
        }
    }

    fn description(&self) -> &'static str {
        "Rewards keeping a `preferLocalBuild` derivation on a worker that already has its inputs, since shipping it elsewhere rarely pays off."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob};
    use gradient_types::ids::OrganizationId;

    fn job(prefer_local_build: bool) -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test",
            OrganizationId::now_v7(),
            "x86_64-linux",
            prefer_local_build,
            false,
            None,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>, missing_count: Option<u32>) -> JobContext<'a> {
        JobContext { job, missing_count, missing_nar_size: None, dependency_count: 0, queued_at: gradient_types::now(), ready_at: gradient_types::now(), org_work_share: None, rescore_count: 0 }
    }

    fn worker() -> WorkerContext<'static> {
        WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: None }
    }

    #[test]
    fn local_worker_with_full_cache_gets_full_bonus() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        assert_eq!(rule.score(&ctx(&j, Some(0)), &worker(), &InstanceContext::default()), rule.local_bonus);
    }

    #[test]
    fn more_missing_paths_lowers_bonus_floored_at_zero() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        let few = rule.score(&ctx(&j, Some(2)), &worker(), &InstanceContext::default());
        let many = rule.score(&ctx(&j, Some(100)), &worker(), &InstanceContext::default());
        assert!(few < rule.local_bonus);
        assert!(many < few);
        assert_eq!(many, 0.0, "deeply-missing closure floors at 0");
    }

    #[test]
    fn unknown_missing_count_is_zero() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        assert_eq!(rule.score(&ctx(&j, None), &worker(), &InstanceContext::default()), 0.0);
    }

    #[test]
    fn not_prefer_local_is_zero_regardless_of_missing_count() {
        let rule = PreferLocalBuildRule::default();
        let j = job(false);
        assert_eq!(rule.score(&ctx(&j, Some(0)), &worker(), &InstanceContext::default()), 0.0);
        assert_eq!(rule.score(&ctx(&j, Some(5)), &worker(), &InstanceContext::default()), 0.0);
        assert_eq!(rule.score(&ctx(&j, None), &worker(), &InstanceContext::default()), 0.0);
    }

    #[test]
    fn knee_tracks_instance_missing_paths() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        let mut inst = InstanceContext::default();
        inst.missing_paths.w1h = 5.0;

        assert_eq!(rule.score(&ctx(&j, Some(10)), &worker(), &inst), 0.0);

        let at_half = rule.score(&ctx(&j, Some(5)), &worker(), &inst);
        assert!(at_half > 0.0);
        assert!(at_half < rule.local_bonus);
    }
}
