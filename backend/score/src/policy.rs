/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::rule::{JobContext, ScoreRule, WorkerContext};
use crate::rules::builtin::{
    BuiltinDeprioritizeRule, DependencyCountRule, MissingNarSizeRule, MissingPathsRule,
    ReserveFetchWorkersRule, WaitTimeRule,
};
use crate::rules::{FairShareRule, PreferLocalBuildRule, ResourceFitRule};

pub trait ScoringPolicy: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64;
    fn uses_history(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct RulePolicy {
    name: &'static str,
    rules: Vec<Box<dyn ScoreRule>>,
    uses_history: bool,
}

impl RulePolicy {
    pub fn new(name: &'static str, rules: Vec<Box<dyn ScoreRule>>, uses_history: bool) -> Self {
        Self { name, rules, uses_history }
    }
}

impl ScoringPolicy for RulePolicy {
    fn name(&self) -> &str {
        self.name
    }

    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        self.rules.iter().map(|r| r.score(job, worker)).sum()
    }

    fn uses_history(&self) -> bool {
        self.uses_history
    }
}

pub fn simple_rules() -> Vec<Box<dyn ScoreRule>> {
    vec![
        Box::new(MissingPathsRule::default()),
        Box::new(MissingNarSizeRule::default()),
        Box::new(DependencyCountRule::default()),
        Box::new(WaitTimeRule::default()),
        Box::new(BuiltinDeprioritizeRule::default()),
        Box::new(ReserveFetchWorkersRule::default()),
    ]
}

pub fn resource_aware_rules() -> Vec<Box<dyn ScoreRule>> {
    let mut rules = simple_rules();
    rules.push(Box::new(ResourceFitRule::default()));
    rules.push(Box::new(PreferLocalBuildRule::default()));
    rules.push(Box::new(FairShareRule::default()));
    rules
}

pub fn policy_by_name(name: &str) -> std::sync::Arc<dyn ScoringPolicy> {
    match name {
        "simple" => std::sync::Arc::new(RulePolicy::new("simple", simple_rules(), false)),
        "resource-aware" => {
            std::sync::Arc::new(RulePolicy::new("resource-aware", resource_aware_rules(), true))
        }
        other => {
            tracing::warn!(policy = other, "unknown scoring policy, using \"resource-aware\"");
            std::sync::Arc::new(RulePolicy::new("resource-aware", resource_aware_rules(), true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{HistoryPrediction, JobKindView, LazyProviders, ScoredJob};
    use gradient_core::types::ids::OrganizationId;
    use gradient_core::types::now;

    fn scored_job(arch: &str) -> ScoredJob<'_> {
        ScoredJob::new(
            "job",
            OrganizationId::now_v7(),
            JobKindView::Build,
            arch,
            false,
            false,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        )
    }

    fn worker_ctx<'a>(archs: &'a [String], feats: &'a [String]) -> WorkerContext<'a> {
        WorkerContext { architectures: archs, system_features: feats, fetch: false, metrics: None }
    }

    #[test]
    fn registry_selects_known_and_falls_back() {
        assert_eq!(policy_by_name("simple").name(), "simple");
        assert_eq!(policy_by_name("resource-aware").name(), "resource-aware");
        assert_eq!(policy_by_name("nonsense").name(), "resource-aware");
    }

    // Anti-starvation (#112): a build waiting an hour must outscore a fresh
    // fully-cached candidate the worker can serve without fetching. Guards the
    // composed simple policy against the WaitTimeRule cap being lowered below
    // the MissingPathsRule scored bonus.
    #[test]
    fn simple_policy_long_waiting_build_overcomes_fresh_cached() {
        let policy = policy_by_name("simple");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);

        let j_fresh = scored_job("x86_64-linux");
        let c_fresh = JobContext {
            job: &j_fresh,
            missing_count: Some(0),
            missing_nar_size: Some(0),
            dependency_count: 0,
            queued_at: now(),
            org_share: None,
        };

        let j_old = scored_job("x86_64-linux");
        let c_old = JobContext {
            job: &j_old,
            missing_count: None,
            missing_nar_size: None,
            dependency_count: 0,
            queued_at: now() - chrono::Duration::seconds(3600),
            org_share: None,
        };

        let s_old = policy.score(&c_old, &w);
        let s_fresh = policy.score(&c_fresh, &w);
        assert!(
            s_old > s_fresh,
            "1-hour-old build must beat fresh fully-cached candidate \
             (anti-starvation): old={s_old} fresh={s_fresh}"
        );
    }

    #[test]
    fn simple_policy_prefers_ready_over_costly() {
        let policy = policy_by_name("simple");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);
        let n = now();

        let j_ready = scored_job("x86_64-linux");
        let c_ready = JobContext {
            job: &j_ready,
            missing_count: Some(0),
            missing_nar_size: Some(0),
            dependency_count: 0,
            queued_at: n,
            org_share: None,
        };

        let j_costly = scored_job("builtin");
        let c_costly = JobContext {
            job: &j_costly,
            missing_count: Some(5),
            missing_nar_size: Some(50_000_000),
            dependency_count: 0,
            queued_at: n,
            org_share: None,
        };

        assert!(policy.score(&c_ready, &w) > policy.score(&c_costly, &w));
    }
}
