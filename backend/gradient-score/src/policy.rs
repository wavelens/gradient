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
    ResourceSaturationRule,
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
            vetoes: Vec::new(),
        }
    }
    fn uses_history(&self) -> bool {
        false
    }
    /// Whether any enabled rule consumes `JobContext::org_work_share`, so the
    /// scheduler skips computing the share otherwise.
    fn uses_org_work_share(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct RulePolicy {
    name: &'static str,
    rules: Vec<Box<dyn ScoreRule>>,
    uses_history: bool,
    uses_org_work_share: bool,
}

impl RulePolicy {
    pub fn new(name: &'static str, rules: Vec<Box<dyn ScoreRule>>, uses_history: bool) -> Self {
        let uses_org_work_share = rules.iter().any(|r| r.uses_org_work_share());
        Self { name, rules, uses_history, uses_org_work_share }
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
        let mut vetoes = Vec::new();
        let mut total = 0.0;
        for r in &self.rules {
            let s = r.score(job, worker, instance);
            total += s;
            rules.insert(r.name().to_string(), s);
            if r.veto(job, worker, instance) {
                vetoes.push(r.name().to_string());
            }
        }
        crate::ScoreBreakdown { rules, total, vetoes }
    }

    fn uses_history(&self) -> bool {
        self.uses_history
    }

    fn uses_org_work_share(&self) -> bool {
        self.uses_org_work_share
    }
}

/// One row of the declarative policy table: the rule and whether the policy
/// ships it enabled. Disabled rules stay compiled, tested, and visible here so
/// their status is an explicit decision instead of a commented-out line.
struct RuleSpec {
    enabled: bool,
    rule: Box<dyn ScoreRule>,
}

fn spec(enabled: bool, rule: Box<dyn ScoreRule>) -> RuleSpec {
    RuleSpec { enabled, rule }
}

fn simple_table() -> Vec<RuleSpec> {
    vec![
        spec(true, Box::new(MissingPathsRule::default())),
        spec(true, Box::new(MissingNarSizeRule::default())),
        spec(true, Box::new(RescoreWaitRule::default())),
        spec(true, Box::new(DependencyCountRule::default())),
        spec(true, Box::new(WaitTimeRule::default())),
        spec(true, Box::new(BuiltinDeprioritizeRule::default())),
        spec(true, Box::new(ReserveFetchWorkersRule::default())),
    ]
}

fn resource_aware_table() -> Vec<RuleSpec> {
    let mut rules = simple_table();
    rules.push(spec(true, Box::new(ResourceFitRule::default())));
    rules.push(spec(true, Box::new(ResourceSaturationRule::default())));
    rules.push(spec(true, Box::new(PreferLocalBuildRule::default())));
    // Disabled: its idle gate counts zero-occupancy rather than spare capacity,
    // over-penalizing busy-but-fair orgs. Re-enabling is a scheduling-policy
    // decision (#476), made here by flipping the flag.
    rules.push(spec(false, Box::new(FairShareRule::default())));
    rules.push(spec(true, Box::new(NetworkAffinityRule::default())));
    rules.push(spec(true, Box::new(DiskAffinityRule::default())));
    rules
}

fn enabled(table: Vec<RuleSpec>) -> Vec<Box<dyn ScoreRule>> {
    table.into_iter().filter(|s| s.enabled).map(|s| s.rule).collect()
}

pub fn simple_rules() -> Vec<Box<dyn ScoreRule>> {
    enabled(simple_table())
}

pub fn resource_aware_rules() -> Vec<Box<dyn ScoreRule>> {
    enabled(resource_aware_table())
}

/// `(name, description)` for every known scoring rule, so the board UI can show
/// what each rule does. Built from the superset policy and deduplicated by name.
pub fn rule_catalog() -> Vec<(&'static str, &'static str)> {
    let mut catalog: Vec<(&'static str, &'static str)> =
        resource_aware_rules().iter().map(|r| (r.name(), r.description())).collect();
    catalog.sort_by_key(|(name, _)| *name);
    catalog.dedup_by_key(|(name, _)| *name);
    catalog
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
    fn rule_catalog_covers_every_rule_with_a_description() {
        let catalog = rule_catalog();
        let rules = resource_aware_rules();

        assert_eq!(catalog.len(), rules.len(), "catalog must list every rule once");
        for (name, description) in &catalog {
            assert!(!name.is_empty(), "rule name must not be empty");
            assert!(!description.is_empty(), "{name} is missing a description");
        }
        for r in &rules {
            assert!(
                catalog.iter().any(|(n, _)| *n == r.name()),
                "{} missing from catalog",
                r.name()
            );
        }
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
            now: now(),
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
            now: now(),
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
            now: now(),
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
            now: now(),
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
            now: now(),
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
            now: now(),
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

    /// Rule names are persisted in `dispatched_job.score_breakdown` and served
    /// by the rule-catalog API: they are a recorded contract. Renaming a rule
    /// struct must not change these strings.
    #[test]
    fn rule_names_are_pinned() {
        let expected = [
            "BuiltinDeprioritizeRule",
            "DependencyCountRule",
            "DiskAffinityRule",
            "MissingNarSizeRule",
            "MissingPathsRule",
            "NetworkAffinityRule",
            "PreferLocalBuildRule",
            "RescoreWaitRule",
            "ReserveFetchWorkersRule",
            "ResourceFitRule",
            "ResourceSaturationRule",
            "WaitTimeRule",
        ];
        let mut got: Vec<&str> = resource_aware_rules().iter().map(|r| r.name()).collect();
        got.sort_unstable();
        assert_eq!(got, expected);
        assert_eq!(FairShareRule::default().name(), "FairShareRule");
    }

    /// An unmeasured build is held by an explicit veto, not by a penalty a
    /// large unrelated bonus could out-vote; the breakdown records who held it.
    #[test]
    fn unmeasured_build_is_vetoed_not_penalized() {
        let policy = policy_by_name("simple");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);
        let j = scored_job("x86_64-linux");
        let held = JobContext {
            job: &j,
            missing_count: None,
            missing_nar_size: None,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            org_work_share: None,
            rescore_count: 0,
            now: now(),
        };

        let breakdown = policy.score_detailed(&held, &w, &InstanceContext::default());
        assert_eq!(breakdown.vetoes, vec!["RescoreWaitRule".to_string()]);
        assert_eq!(breakdown.rules["RescoreWaitRule"], 0.0);
    }

    /// Only FairShareRule consumes org_work_share, and it ships disabled, so
    /// the live policies must not ask the scheduler to compute the share.
    #[test]
    fn org_work_share_is_unconsumed_while_fair_share_is_disabled() {
        assert!(!policy_by_name("simple").uses_org_work_share());
        assert!(!policy_by_name("resource-aware").uses_org_work_share());
        assert!(FairShareRule::default().uses_org_work_share());
    }
}
