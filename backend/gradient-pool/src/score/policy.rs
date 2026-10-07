/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::score::context::InstanceContext;
use crate::score::rule::{JobContext, ScoreRule, WorkerContext};
use crate::score::rules::{
    EstimatedTimeRule, FairShareRule, QosRule, RescoreWaitRule, ReserveFetchWorkersRule,
    ResourceSaturationRule, TransferLimitRule, WaitTimeRule,
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
    ) -> crate::score::ScoreBreakdown {
        crate::score::ScoreBreakdown {
            rules: std::collections::BTreeMap::new(),
            total: self.score(job, worker, instance),
            vetoes: Vec::new(),
            estimate: None,
        }
    }
    fn uses_history(&self) -> bool {
        false
    }
    fn uses_project_work_share(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct RulePolicy {
    name: &'static str,
    rules: Vec<Box<dyn ScoreRule>>,
    uses_history: bool,
    uses_project_work_share: bool,
}

impl RulePolicy {
    pub fn new(name: &'static str, rules: Vec<Box<dyn ScoreRule>>, uses_history: bool) -> Self {
        let uses_project_work_share = rules.iter().any(|r| r.uses_project_work_share());
        Self {
            name,
            rules,
            uses_history,
            uses_project_work_share,
        }
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
        self.rules
            .iter()
            .map(|r| r.score(job, worker, instance))
            .sum()
    }

    fn score_detailed(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> crate::score::ScoreBreakdown {
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
        crate::score::ScoreBreakdown {
            rules,
            total,
            vetoes,
            estimate: Some(crate::score::rules::estimated_time::estimate(
                job, worker, instance,
            )),
        }
    }

    fn uses_history(&self) -> bool {
        self.uses_history
    }

    fn uses_project_work_share(&self) -> bool {
        self.uses_project_work_share
    }
}

struct RuleSpec {
    enabled: bool,
    rule: Box<dyn ScoreRule>,
}

fn spec(enabled: bool, rule: Box<dyn ScoreRule>) -> RuleSpec {
    RuleSpec { enabled, rule }
}

fn simple_table() -> Vec<RuleSpec> {
    vec![
        spec(true, Box::new(EstimatedTimeRule::default())),
        spec(true, Box::new(RescoreWaitRule::default())),
        spec(true, Box::new(WaitTimeRule::default())),
        spec(true, Box::new(ReserveFetchWorkersRule::default())),
        spec(true, Box::new(TransferLimitRule::default())),
        spec(true, Box::new(QosRule::default())),
    ]
}

fn resource_aware_table() -> Vec<RuleSpec> {
    let mut rules = simple_table();
    rules.push(spec(true, Box::new(ResourceSaturationRule::default())));
    // FairShareRule is disabled because its idle gate is counting zero occupancy, not spare
    // capacity. Re-enabling it is a scheduling-policy decision (#476).
    rules.push(spec(false, Box::new(FairShareRule::default())));
    rules
}

fn enabled(table: Vec<RuleSpec>) -> Vec<Box<dyn ScoreRule>> {
    table
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| s.rule)
        .collect()
}

pub fn simple_rules() -> Vec<Box<dyn ScoreRule>> {
    enabled(simple_table())
}

pub fn resource_aware_rules() -> Vec<Box<dyn ScoreRule>> {
    enabled(resource_aware_table())
}

pub fn rule_catalog() -> Vec<(&'static str, &'static str)> {
    let mut catalog: Vec<(&'static str, &'static str)> = resource_aware_table()
        .iter()
        .map(|s| (s.rule.name(), s.rule.description()))
        .collect();
    catalog.sort_by_key(|(name, _)| *name);
    catalog.dedup_by_key(|(name, _)| *name);
    catalog
}

pub fn policy_by_name(name: &str) -> std::sync::Arc<dyn ScoringPolicy> {
    match name {
        "simple" => std::sync::Arc::new(RulePolicy::new("simple", simple_rules(), false)),
        "resource-aware" => std::sync::Arc::new(RulePolicy::new(
            "resource-aware",
            resource_aware_rules(),
            true,
        )),
        other => {
            tracing::warn!(
                policy = other,
                "unknown scoring policy, using \"resource-aware\""
            );
            std::sync::Arc::new(RulePolicy::new(
                "resource-aware",
                resource_aware_rules(),
                true,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::context::{HistoryPrediction, ScoredJob};
    use gradient_types::ids::ProjectId;
    use gradient_types::now;

    fn scored_job(arch: &str) -> ScoredJob<'_> {
        ScoredJob::new_build(
            "job",
            ProjectId::now_v7(),
            arch,
            false,
            false,
            None,
            None,
            HistoryPrediction::default(),
        )
    }

    fn worker_ctx<'a>(archs: &'a [String], feats: &'a [String]) -> WorkerContext<'a> {
        WorkerContext {
            architectures: archs,
            system_features: feats,
            fetch: false,
            metrics: None,
        }
    }

    #[test]
    fn rule_catalog_covers_every_rule_with_a_description() {
        let catalog = rule_catalog();
        let rules: Vec<_> = resource_aware_table().into_iter().map(|s| s.rule).collect();

        assert_eq!(
            catalog.len(),
            rules.len(),
            "catalog must list every rule once"
        );
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
            outputs_present: false,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };

        let j_old = scored_job("x86_64-linux");
        let c_old = JobContext {
            job: &j_old,
            missing_count: None,
            missing_nar_size: None,
            outputs_present: false,
            dependency_count: 0,
            queued_at: now() - chrono::Duration::seconds(3600),
            ready_at: now() - chrono::Duration::seconds(3600),
            project_work_share: None,
            prioritized: false,
            build_request: false,
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
    fn resource_aware_sends_heavy_build_to_fast_cold_worker_over_slow_warm_one() {
        use crate::score::context::WorkerMetricsView;
        let policy = policy_by_name("resource-aware");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let j = ScoredJob::new_build(
            "j",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            None,
            HistoryPrediction {
                build_time_ms: Some(20 * 60_000),
                uncontended_build_time_ms: Some(20 * 60_000),
                build_core_score: Some(10_000),
                samples: 5,
                ..Default::default()
            },
        );
        let on = |missing_count, missing_nar_size| JobContext {
            job: &j,
            missing_count: Some(missing_count),
            missing_nar_size: Some(missing_nar_size),
            outputs_present: false,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };
        let with_cores = |cpu_core_score| WorkerContext {
            architectures: &archs,
            system_features: &feats,
            fetch: false,
            metrics: Some(WorkerMetricsView {
                cpu_core_score,
                ..Default::default()
            }),
        };
        let inst = InstanceContext {
            cpu_core_score_mean: Some(10_000.0),
            ..Default::default()
        };

        let warm_slow = policy.score(&on(0, 0), &with_cores(5_000), &inst);
        let cold_fast = policy.score(&on(40, 4 << 30), &with_cores(15_000), &inst);
        assert!(
            cold_fast > warm_slow,
            "cold_fast={cold_fast} warm_slow={warm_slow}"
        );
    }

    #[test]
    fn resource_aware_sends_heavy_build_to_the_worker_holding_its_outputs() {
        use crate::score::context::WorkerMetricsView;
        let policy = policy_by_name("resource-aware");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let j = ScoredJob::new_build(
            "j",
            ProjectId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            None,
            HistoryPrediction {
                build_time_ms: Some(30 * 60_000),
                uncontended_build_time_ms: Some(30 * 60_000),
                predicted_peak_ram_mb: Some(64_000),
                samples: 5,
                ..Default::default()
            },
        );
        let on = |outputs_present, missing_count, missing_nar_size| JobContext {
            job: &j,
            missing_count: Some(missing_count),
            missing_nar_size: Some(missing_nar_size),
            outputs_present,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };
        let with_cores = |cpu_core_score, ram_free_mb| WorkerContext {
            architectures: &archs,
            system_features: &feats,
            fetch: false,
            metrics: Some(WorkerMetricsView {
                cpu_core_score,
                ram_total_mb: 128_000,
                ram_free_mb: Some(ram_free_mb),
                ..Default::default()
            }),
        };
        let inst = InstanceContext {
            cpu_core_score_mean: Some(10_000.0),
            ..Default::default()
        };

        let holding_slow = policy.score(&on(true, 40, 4 << 30), &with_cores(2_000, 16_000), &inst);
        let cold_fast = policy.score(&on(false, 0, 0), &with_cores(20_000, 120_000), &inst);
        assert!(
            holding_slow > cold_fast,
            "holding_slow={holding_slow} cold_fast={cold_fast}"
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
            outputs_present: false,
            dependency_count: 0,
            queued_at: n,
            ready_at: n,
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };

        let j_costly = scored_job("builtin");
        let c_costly = JobContext {
            job: &j_costly,
            missing_count: Some(5),
            missing_nar_size: Some(50_000_000),
            outputs_present: false,
            dependency_count: 0,
            queued_at: n,
            ready_at: n,
            project_work_share: None,
            prioritized: false,
            build_request: false,
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
            outputs_present: false,
            dependency_count: 2,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };

        let breakdown = policy.score_detailed(&c, &w, &InstanceContext::default());
        let total = policy.score(&c, &w, &InstanceContext::default());

        assert!(
            (breakdown.total - total).abs() < 1e-9,
            "total must match score()"
        );
        assert_eq!(breakdown.rules.len(), 6, "simple policy has 6 rules");
        assert!(breakdown.rules.contains_key("EstimatedTimeRule"));
        assert!(breakdown.rules.contains_key("QosRule"));
        assert!(breakdown.rules.contains_key("WaitTimeRule"));
        let sum: f64 = breakdown.rules.values().sum();
        assert!(
            (sum - total).abs() < 1e-9,
            "rule contributions must sum to total"
        );

        let estimate = breakdown
            .estimate
            .as_ref()
            .expect("the rule policy records the estimate");
        let rule = crate::score::rules::EstimatedTimeRule::default();
        assert_eq!(
            breakdown.rules["EstimatedTimeRule"],
            rule.points_per_sec * (rule.cap_secs - estimate.total().min(rule.cap_secs)),
            "the stored estimate matches the rule's score"
        );
    }

    /// Rule names are persisted in `dispatched_job.score_breakdown` and served by the rule-catalog
    /// API. Renaming a rule struct must not change these strings.
    #[test]
    fn rule_names_are_pinned() {
        let expected = [
            "EstimatedTimeRule",
            "QosRule",
            "RescoreWaitRule",
            "ReserveFetchWorkersRule",
            "ResourceSaturationRule",
            "TransferLimitRule",
            "WaitTimeRule",
        ];
        let mut got: Vec<&str> = resource_aware_rules().iter().map(|r| r.name()).collect();
        got.sort_unstable();
        assert_eq!(got, expected);
        assert_eq!(FairShareRule::default().name(), "FairShareRule");
    }

    #[test]
    fn a_held_transfer_drops_below_the_floor_until_its_wait_releases_it() {
        let policy = policy_by_name("simple");
        let archs = vec!["x86_64-linux".to_string()];
        let feats: Vec<String> = vec![];
        let w = worker_ctx(&archs, &feats);
        let j = scored_job("x86_64-linux");
        let waited = |secs| JobContext {
            job: &j,
            missing_count: Some(0),
            missing_nar_size: Some(4 << 30),
            outputs_present: false,
            dependency_count: 0,
            queued_at: now() - chrono::Duration::seconds(secs),
            ready_at: now() - chrono::Duration::seconds(secs),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };
        let full = InstanceContext {
            downloads_in_flight: 16,
            download_slots: 16,
            ..Default::default()
        };

        assert!(policy.score(&waited(0), &w, &full) < crate::score::weights::ASSIGN_FLOOR);
        assert!(policy.score(&waited(36_000), &w, &full) >= crate::score::weights::ASSIGN_FLOOR);
    }

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
            outputs_present: false,
            dependency_count: 0,
            queued_at: now(),
            ready_at: now(),
            project_work_share: None,
            prioritized: false,
            build_request: false,
            rescore_count: 0,
            now: now(),
        };

        let breakdown = policy.score_detailed(&held, &w, &InstanceContext::default());
        assert_eq!(breakdown.vetoes, vec!["RescoreWaitRule".to_string()]);
        assert_eq!(breakdown.rules["RescoreWaitRule"], 0.0);
    }

    #[test]
    fn project_work_share_is_unconsumed_while_fair_share_is_disabled() {
        assert!(!policy_by_name("simple").uses_project_work_share());
        assert!(!policy_by_name("resource-aware").uses_project_work_share());
        assert!(FairShareRule::default().uses_project_work_share());
    }
}
