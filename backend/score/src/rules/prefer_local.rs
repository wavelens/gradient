/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
    fn score(&self, job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        if !job.job.prefer_local_build {
            return 0.0;
        }
        match job.missing_count {
            Some(0) => self.local_bonus,
            Some(n) => (self.local_bonus - n as f64 * self.miss_penalty).max(0.0),
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, JobKindView, LazyProviders, ScoredJob};
    use gradient_core::types::ids::OrganizationId;

    fn job(prefer_local_build: bool) -> ScoredJob<'static> {
        ScoredJob::new(
            "test",
            OrganizationId::now_v7(),
            JobKindView::Build,
            "x86_64-linux",
            prefer_local_build,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        )
    }

    fn ctx<'a>(job: &'a ScoredJob<'a>, missing_count: Option<u32>) -> JobContext<'a> {
        JobContext { job, missing_count, missing_nar_size: None, dependency_count: 0, queued_at: gradient_core::types::now(), org_share: None }
    }

    fn worker() -> WorkerContext<'static> {
        WorkerContext { architectures: &[], system_features: &[], fetch: false, metrics: None }
    }

    #[test]
    fn local_worker_with_full_cache_gets_full_bonus() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        assert_eq!(rule.score(&ctx(&j, Some(0)), &worker()), rule.local_bonus);
    }

    #[test]
    fn more_missing_paths_lowers_bonus_floored_at_zero() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        let few = rule.score(&ctx(&j, Some(2)), &worker());
        let many = rule.score(&ctx(&j, Some(100)), &worker());
        assert!(few < rule.local_bonus);
        assert!(many < few);
        assert_eq!(many, 0.0, "deeply-missing closure floors at 0");
    }

    #[test]
    fn unknown_missing_count_is_zero() {
        let rule = PreferLocalBuildRule::default();
        let j = job(true);
        assert_eq!(rule.score(&ctx(&j, None), &worker()), 0.0);
    }

    #[test]
    fn not_prefer_local_is_zero_regardless_of_missing_count() {
        let rule = PreferLocalBuildRule::default();
        let j = job(false);
        assert_eq!(rule.score(&ctx(&j, Some(0)), &worker()), 0.0);
        assert_eq!(rule.score(&ctx(&j, Some(5)), &worker()), 0.0);
        assert_eq!(rule.score(&ctx(&j, None), &worker()), 0.0);
    }
}
