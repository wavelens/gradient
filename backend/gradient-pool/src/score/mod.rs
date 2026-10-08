/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod breakdown;
pub mod context;
pub mod policy;
pub mod rule;
pub mod rules;
pub mod weights;

pub use breakdown::ScoreBreakdown;
pub use context::{
    BuildContext, DerivationRef, EvalContext, HistoryPrediction, InstanceContext, JobKindContext,
    ScoredBuild, ScoredJob, SubstituteCost, Windowed, WorkerMetricsView,
};
pub use policy::{RulePolicy, ScoringPolicy, policy_by_name, rule_catalog};
pub use rule::{JobContext, ScoreRule, WorkerContext};
pub use rules::estimated_time::contention_factor;
