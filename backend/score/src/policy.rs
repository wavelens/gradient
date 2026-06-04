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
}

#[derive(Debug)]
pub struct RulePolicy {
    name: &'static str,
    rules: Vec<Box<dyn ScoreRule>>,
}

impl RulePolicy {
    pub fn new(name: &'static str, rules: Vec<Box<dyn ScoreRule>>) -> Self {
        Self { name, rules }
    }
}

impl ScoringPolicy for RulePolicy {
    fn name(&self) -> &str {
        self.name
    }

    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64 {
        self.rules.iter().map(|r| r.score(job, worker)).sum()
    }
}

pub fn default_rules() -> Vec<Box<dyn ScoreRule>> {
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
    let mut rules = default_rules();
    rules.push(Box::new(ResourceFitRule));
    rules.push(Box::new(PreferLocalBuildRule));
    rules.push(Box::new(FairShareRule));
    rules
}

pub fn policy_by_name(name: &str) -> std::sync::Arc<dyn ScoringPolicy> {
    match name {
        "default" => std::sync::Arc::new(RulePolicy::new("default", default_rules())),
        "resource-aware" => std::sync::Arc::new(RulePolicy::new("resource-aware", resource_aware_rules())),
        other => {
            tracing::warn!(policy = other, "unknown scoring policy, using \"default\"");
            std::sync::Arc::new(RulePolicy::new("default", default_rules()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_selects_known_and_falls_back() {
        assert_eq!(policy_by_name("default").name(), "default");
        assert_eq!(policy_by_name("resource-aware").name(), "resource-aware");
        assert_eq!(policy_by_name("nonsense").name(), "default");
    }
}
