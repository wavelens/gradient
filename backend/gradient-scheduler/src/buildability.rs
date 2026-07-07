/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pre-loaded derivation/feature data for a set of pending build anchors, used
//! to decide whether the connected worker pool can build any of them.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result};

use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::dispatch_mode::{BuildDispatchMode, arch_available, decide_dispatch_mode};

/// Pre-loaded derivation and feature data for a set of pending anchors.
///
/// Used by [`crate::waiting_state::reconcile_waiting_state`] to determine
/// whether any pending anchor can be satisfied by the current worker pool
/// without re-querying the DB per evaluation.
pub(crate) struct BuildabilityChecker {
    drv_by_id: HashMap<DerivationId, MDerivation>,
    /// derivation_build → `SubstituteUnavailable` miss count. A substitutable
    /// anchor is only treated as buildable-anywhere while it is below the
    /// escalation threshold; past it, it is checked against real arch/features
    /// like any other anchor (so the parker can park it when no arch worker exists).
    substitute_misses: HashMap<DerivationBuildId, i64>,
    substitute_miss_escalation_threshold: i64,
    /// Maps derivation ID → list of required feature IDs.
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_name: HashMap<FeatureId, String>,
    connected_architectures: std::collections::HashSet<String>,
}

impl BuildabilityChecker {
    /// Query the DB for all derivations, required features, and substitute-miss
    /// counts referenced by `anchors`, returning a checker ready to call
    /// [`any_buildable`].
    ///
    /// [`any_buildable`]: BuildabilityChecker::any_buildable
    pub(crate) async fn load(
        state: &Arc<ServerState>,
        anchors: &[MDerivationBuild],
        connected_architectures: std::collections::HashSet<String>,
        evaluation_id: EvaluationId,
    ) -> Result<Self> {
        let db = &state.worker_db;
        let drv_ids: Vec<DerivationId> = anchors.iter().map(|a| a.derivation).collect();
        let anchor_ids: Vec<DerivationBuildId> = anchors.iter().map(|a| a.id).collect();
        // A count-query failure → 0 misses → substitute-mode, same as the dispatch side.
        // Scoped to this evaluation so a fresh eval is not parked on a previous
        // eval's exhausted substitute budget.
        let substitute_misses = gradient_db::substitute_miss_counts(db, &anchor_ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|((anchor, eval), misses)| {
                (eval == evaluation_id).then_some((anchor, misses))
            })
            .collect();

        let drvs = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("fetch derivations for pending builds")?;
        let drv_by_id: HashMap<DerivationId, MDerivation> =
            drvs.into_iter().map(|d| (d.id, d)).collect();

        let edges = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationFeature::find()
                .filter(CDerivationFeature::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("fetch derivation_feature edges")?;
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        for e in &edges {
            features_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.feature);
        }

        let feature_ids: Vec<FeatureId> = edges.iter().map(|e| e.feature).collect();
        let feature_rows = gradient_db::fetch_in_chunks(&feature_ids, |chunk| async move {
            EFeature::find()
                .filter(CFeature::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("fetch feature names")?;
        let feature_name: HashMap<FeatureId, String> =
            feature_rows.into_iter().map(|f| (f.id, f.name)).collect();

        Ok(Self {
            drv_by_id,
            substitute_misses,
            substitute_miss_escalation_threshold: state
                .config
                .eval
                .substitute_miss_escalation_threshold
                as i64,
            features_by_drv,
            feature_name,
            connected_architectures,
        })
    }

    /// Whether any pending anchor can run on the connected pool. A `Queued` or
    /// `Building` anchor already has all dependency anchors terminal-success
    /// (the promotion invariant), so it is dispatchable; a `Created` anchor is
    /// still blocked on deps. Substitutable anchors run anywhere until they
    /// exhaust the miss budget.
    pub(crate) fn any_buildable(
        &self,
        anchors: &[MDerivationBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> bool {
        anchors.iter().any(|a| {
            if a.status == BuildStatus::Building {
                return true;
            }
            if a.status != BuildStatus::Queued {
                return false;
            }
            let Some(drv) = self.drv_by_id.get(&a.derivation) else {
                return false;
            };
            let miss = self.substitute_misses.get(&a.id).copied().unwrap_or(0);
            let arch_has_worker = arch_available(&self.connected_architectures, &drv.architecture);
            match decide_dispatch_mode(
                a.substitutable,
                miss,
                self.substitute_miss_escalation_threshold,
                arch_has_worker,
            ) {
                BuildDispatchMode::SubstituteBuiltin => true,
                BuildDispatchMode::SubstituteStalled => false,
                BuildDispatchMode::RealArch => {
                    let required: Vec<&str> = self.required_features_for(&a.derivation);
                    worker_caps.iter().any(|(arch, feats)| {
                        let arch_ok = drv.architecture == gradient_types::BUILTIN_ARCH
                            || arch.iter().any(|a| a == &drv.architecture);
                        let feats_ok = required.iter().all(|f| feats.iter().any(|sf| sf == f));
                        arch_ok && feats_ok
                    })
                }
            }
        })
    }

    fn required_features_for(&self, drv_id: &DerivationId) -> Vec<&str> {
        self.features_by_drv
            .get(drv_id)
            .map(|ids| {
                let mut names: Vec<&str> = ids
                    .iter()
                    .filter_map(|i| self.feature_name.get(i).map(String::as_str))
                    .collect();
                names.sort_unstable();
                names.dedup();
                names
            })
            .unwrap_or_default()
    }

    /// Group every unsatisfiable `(architecture, required_features)` combo and
    /// the number of pending anchors it covers. Used for the API
    /// `waiting_reason` payload so the UI can explain *why* nothing is
    /// dispatching.
    pub(crate) fn compute_waiting_reason(
        &self,
        anchors: &[MDerivationBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> WaitingReason {
        let mut grouped: BTreeMap<(String, Vec<String>), u32> = BTreeMap::new();
        for a in anchors {
            let miss = self.substitute_misses.get(&a.id).copied().unwrap_or(0);
            let arch_has_worker = self
                .drv_by_id
                .get(&a.derivation)
                .map(|d| arch_available(&self.connected_architectures, &d.architecture))
                .unwrap_or(false);
            if matches!(
                decide_dispatch_mode(
                    a.substitutable,
                    miss,
                    self.substitute_miss_escalation_threshold,
                    arch_has_worker
                ),
                BuildDispatchMode::SubstituteBuiltin
            ) {
                continue;
            }
            let Some(drv) = self.drv_by_id.get(&a.derivation) else {
                continue;
            };
            let required_owned: Vec<String> = self
                .required_features_for(&a.derivation)
                .into_iter()
                .map(str::to_owned)
                .collect();
            let satisfied = worker_caps.iter().any(|(arch, feats)| {
                let arch_ok = drv.architecture == gradient_types::BUILTIN_ARCH
                    || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required_owned
                    .iter()
                    .all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            });
            if satisfied {
                continue;
            }
            *grouped
                .entry((drv.architecture.clone(), required_owned))
                .or_default() += 1;
        }

        let unmet: Vec<UnmetRequirement> = grouped
            .into_iter()
            .map(
                |((architecture, required_features), build_count)| UnmetRequirement {
                    architecture,
                    required_features,
                    build_count,
                },
            )
            .collect();

        let mut available_architectures: Vec<String> = worker_caps
            .iter()
            .flat_map(|(archs, _)| archs.iter().cloned())
            .collect();
        available_architectures.sort_unstable();
        available_architectures.dedup();

        WaitingReason::Workers {
            unmet,
            connected_workers: worker_caps.len() as u32,
            available_architectures,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workers_view(r: &WaitingReason) -> (&[UnmetRequirement], u32, &[String]) {
        match r {
            WaitingReason::Workers {
                unmet,
                connected_workers,
                available_architectures,
            } => (unmet, *connected_workers, available_architectures),
            other => panic!("expected Workers variant, got {other:?}"),
        }
    }

    fn drv(id: DerivationId, arch: &str) -> MDerivation {
        gradient_entity::derivation::Model {
            id,
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "x".into(),
            architecture: arch.into(),
            created_at: chrono::NaiveDateTime::default(),
            ..Default::default()
        }
    }

    fn build_for(drv_id: DerivationId, _eval_id: EvaluationId) -> MDerivationBuild {
        gradient_entity::derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: drv_id,
            status: BuildStatus::Queued,
            ..Default::default()
        }
    }

    fn checker_with(
        drvs: Vec<MDerivation>,
        feature_edges: Vec<(DerivationId, FeatureId, &'static str)>,
    ) -> BuildabilityChecker {
        let drv_by_id = drvs.into_iter().map(|d| (d.id, d)).collect();
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        let mut feature_name: HashMap<FeatureId, String> = HashMap::new();
        for (drv_id, feat_id, name) in feature_edges {
            features_by_drv.entry(drv_id).or_default().push(feat_id);
            feature_name.insert(feat_id, name.to_string());
        }
        BuildabilityChecker {
            drv_by_id,
            substitute_misses: HashMap::new(),
            substitute_miss_escalation_threshold: 2,
            features_by_drv,
            feature_name,
            connected_architectures: std::collections::HashSet::new(),
        }
    }

    #[test]
    fn no_workers_lists_every_unique_arch() {
        let eval_id = EvaluationId::now_v7();
        let d1 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d2 = drv(DerivationId::now_v7(), "x86_64-linux");
        let builds = vec![build_for(d1.id, eval_id), build_for(d2.id, eval_id)];
        let checker = checker_with(vec![d1, d2], vec![]);

        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);

        assert_eq!(connected_workers, 0);
        assert!(available_architectures.is_empty());
        assert_eq!(unmet.len(), 2);
        assert!(
            unmet
                .iter()
                .any(|u| u.architecture == "aarch64-linux" && u.build_count == 1)
        );
        assert!(
            unmet
                .iter()
                .any(|u| u.architecture == "x86_64-linux" && u.build_count == 1)
        );
    }

    #[test]
    fn satisfied_builds_are_excluded_from_unmet() {
        let eval_id = EvaluationId::now_v7();
        let d_x86 = drv(DerivationId::now_v7(), "x86_64-linux");
        let d_arm = drv(DerivationId::now_v7(), "aarch64-linux");
        let builds = vec![build_for(d_x86.id, eval_id), build_for(d_arm.id, eval_id)];
        let checker = checker_with(vec![d_x86, d_arm], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);

        assert_eq!(connected_workers, 1);
        assert_eq!(available_architectures, ["x86_64-linux"]);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
        assert_eq!(unmet[0].build_count, 1);
    }

    #[test]
    fn missing_feature_is_reported_alongside_arch() {
        let eval_id = EvaluationId::now_v7();
        let drv_id = DerivationId::now_v7();
        let feat_id = FeatureId::now_v7();
        let d = drv(drv_id, "x86_64-linux");
        let builds = vec![build_for(drv_id, eval_id)];
        let checker = checker_with(vec![d], vec![(drv_id, feat_id, "kvm")]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);

        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "x86_64-linux");
        assert_eq!(unmet[0].required_features, vec!["kvm".to_string()]);
        assert_eq!(unmet[0].build_count, 1);
    }

    #[test]
    fn identical_requirements_are_grouped_with_count() {
        let eval_id = EvaluationId::now_v7();
        let d1 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d2 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d3 = drv(DerivationId::now_v7(), "aarch64-linux");
        let builds = vec![
            build_for(d1.id, eval_id),
            build_for(d2.id, eval_id),
            build_for(d3.id, eval_id),
        ];
        let checker = checker_with(vec![d1, d2, d3], vec![]);

        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, _, _) = workers_view(&reason);

        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
        assert_eq!(unmet[0].build_count, 3);
    }

    #[test]
    fn builtin_arch_satisfied_by_any_worker() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "builtin");
        let builds = vec![build_for(d.id, eval_id)];
        let checker = checker_with(vec![d], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);

        assert!(unmet.is_empty());
    }

    fn substitutable_build(drv_id: DerivationId, _eval_id: EvaluationId) -> MDerivationBuild {
        gradient_entity::derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: drv_id,
            status: BuildStatus::Queued,
            substitutable: true,
            ..Default::default()
        }
    }

    #[test]
    fn substitutable_below_threshold_is_buildable_anywhere() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "aarch64-linux");
        let build = substitutable_build(d.id, eval_id);
        let mut checker = checker_with(vec![d], vec![]);
        checker.substitute_misses.insert(build.id, 1);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let builds = [build];
        assert!(checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert!(unmet.is_empty());
    }

    #[test]
    fn substitutable_at_threshold_escalates_to_real_arch_check() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "aarch64-linux");
        let build = substitutable_build(d.id, eval_id);
        let mut checker = checker_with(vec![d], vec![]);
        checker.substitute_misses.insert(build.id, 2);

        // No aarch64 worker: the escalated build is no longer buildable-anywhere
        // and surfaces as an unmet aarch64 requirement so the parker can park it.
        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let builds = [build];
        assert!(!checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
    }

    #[test]
    fn stalled_substitute_is_not_buildable_and_appears_in_unmet() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "i686-linux");
        let mut b = build_for(d.id, eval_id);
        b.substitutable = true;
        let mut checker = checker_with(vec![d.clone()], vec![]);
        checker.substitute_misses.insert(b.id, 2);
        checker
            .connected_architectures
            .insert("x86_64-linux".into());
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(!checker.any_buildable(&[b.clone()], &caps));
        let reason = checker.compute_waiting_reason(&[b], &caps);
        let (unmet, _, available) = workers_view(&reason);
        assert!(unmet.iter().any(|u| u.architecture == "i686-linux"));
        assert_eq!(available, ["x86_64-linux"]);
    }

    #[test]
    fn dependency_blocked_anchor_is_not_buildable() {
        // A `Created` anchor still has unsatisfied dependency anchors, so it is
        // not dispatchable even when a matching worker is connected.
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "x86_64-linux");
        let mut b = build_for(d.id, eval_id);
        b.status = BuildStatus::Created;
        let mut checker = checker_with(vec![d], vec![]);
        checker
            .connected_architectures
            .insert("x86_64-linux".into());
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(!checker.any_buildable(&[b], &caps));
    }

    #[test]
    fn substitutable_within_budget_is_buildable_anywhere() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "i686-linux");
        let mut b = build_for(d.id, eval_id);
        b.substitutable = true;
        let checker = checker_with(vec![d], vec![]);
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(checker.any_buildable(&[b], &caps));
    }
}
