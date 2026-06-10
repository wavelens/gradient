/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::InstanceContext;
use crate::rule::{JobContext, ScoreRule, WorkerContext};
use crate::rules::builtin::{
    BuiltinDeprioritizeRule, DependencyCountRule, MissingNarSizeRule, MissingPathsRule,
    ReserveFetchWorkersRule, RescoreWaitRule, WaitTimeRule,
};
use crate::rules::{
    DiskAffinityRule, FairShareRule, NetworkAffinityRule, PreferLocalBuildRule, ResourceFitRule,
};

pub trait ScoringPolicy: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64;
    fn score_detailed(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> crate::ScoreBreakdown {
        crate::ScoreBreakdown {
            rules: std::collections::BTreeMap::new(),
            total: self.score(job, worker, instance),
        }
    }
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

    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64 {
        self.rules.iter().map(|r| r.score(job, worker, instance)).sum()
    }

    fn score_detailed(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> crate::ScoreBreakdown {
        let mut rules = std::collections::BTreeMap::new();
        let mut total = 0.0;
        for r in &self.rules {
            let s = r.score(job, worker, instance);
            total += s;
            rules.insert(r.name().to_string(), s);
        }
        crate::ScoreBreakdown { rules, total }
    }

    fn uses_history(&self) -> bool {
        self.uses_history
    }
}

pub fn simple_rules() -> Vec<Box<dyn ScoreRule>> {
    vec![
        Box::new(MissingPathsRule::default()),
        Box::new(MissingNarSizeRule::default()),
        Box::new(RescoreWaitRule::default()),
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
    rules.push(Box::new(NetworkAffinityRule::default()));
    rules.push(Box::new(DiskAffinityRule::default()));
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
    use crate::context::{HistoryPrediction, LazyProviders, ScoredJob};
    use gradient_types::ids::OrganizationId;
    use gradient_types::now;

    fn scored_job(arch: &str) -> ScoredJob<'_> {
        ScoredJob::new_build(
            "job",
            OrganizationId::now_v7(),
            arch,
            false,
            false,
            None,
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
            ready_at: now(),
            org_work_share: None,
            rescore_count: 0,
        };

        let j_old = scored_job("x86_64-linux");
        let c_old = JobContext {
            job: &j_old,
            missing_count: None,
            missing_nar_size: None,
            dependency_count: 0,
            queued_at: now() - chrono::Duration::seconds(3600),
            ready_at: now() - chrono::Duration::seconds(3600),
            org_work_share: None,
            rescore_count: 0,
        };

        let s_old = policy.score(&c_old, &w, &InstanceContext::default());
        let s_fresh = policy.score(&c_fresh, &w, &InstanceContext::default());
        assert!(
            s_old > s_fresh,
            "1-hour-old build must beat fresh fully-cached candidate \
             (anti-starvation): old={s_old} fresh={s_fresh}"
        );
    }

    #[test]
    fn resource_aware_prefers_fast_net_for_fod() {
        use crate::context::WorkerMetricsView;
        let policy = policy_by_name("resource-aware");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let j = ScoredJob::new_build(
            "j",
            OrganizationId::now_v7(),
            "x86_64-linux",
            false,
            true,
            None,
            LazyProviders { closure_size: &|| None, history: &|| HistoryPrediction::default() },
        );
        let c = JobContext {
            job: &j,
            missing_count: Some(0),
            missing_nar_size: Some(0),
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            org_work_share: None,
            rescore_count: 0,
        };
        let fast = WorkerContext {
            architectures: &archs,
            system_features: &feats,
            fetch: false,
            metrics: Some(WorkerMetricsView { network_speed_mbps: Some(100.0), ..Default::default() }),
        };
        let slow = WorkerContext {
            architectures: &archs,
            system_features: &feats,
            fetch: false,
            metrics: Some(WorkerMetricsView { network_speed_mbps: Some(5.0), ..Default::default() }),
        };
        assert!(
            policy.score(&c, &fast, &InstanceContext::default())
                > policy.score(&c, &slow, &InstanceContext::default())
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
            ready_at: n,
            org_work_share: None,
            rescore_count: 0,
        };

        let j_costly = scored_job("builtin");
        let c_costly = JobContext {
            job: &j_costly,
            missing_count: Some(5),
            missing_nar_size: Some(50_000_000),
            dependency_count: 0,
            queued_at: n,
            ready_at: n,
            org_work_share: None,
            rescore_count: 0,
        };

        assert!(
            policy.score(&c_ready, &w, &InstanceContext::default())
                > policy.score(&c_costly, &w, &InstanceContext::default())
        );
    }

    #[test]
    fn score_detailed_sums_to_total_and_names_rules() {
        let policy = policy_by_name("simple");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);
        let j = scored_job("x86_64-linux");
        let c = JobContext {
            job: &j,
            missing_count: Some(0),
            missing_nar_size: Some(0),
            dependency_count: 2,
            queued_at: now(),
            ready_at: now(),
            org_work_share: None,
            rescore_count: 0,
        };

        let breakdown = policy.score_detailed(&c, &w, &InstanceContext::default());
        let total = policy.score(&c, &w, &InstanceContext::default());

        assert!((breakdown.total - total).abs() < 1e-9, "total must match score()");
        assert_eq!(breakdown.rules.len(), 7, "simple policy has 7 rules");
        assert!(breakdown.rules.contains_key("MissingPathsRule"));
        assert!(breakdown.rules.contains_key("WaitTimeRule"));
        let sum: f64 = breakdown.rules.values().sum();
        assert!((sum - total).abs() < 1e-9, "rule contributions must sum to total");
    }
}
