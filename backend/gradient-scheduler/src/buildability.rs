/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result};

use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::assign_mode::decide_build_spec_kind;
use gradient_wire::types::BuildSpecKind;

pub(crate) struct BuildabilityChecker {
    drv_by_id: HashMap<DerivationId, MDerivation>,
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_name: HashMap<FeatureId, String>,
}

impl BuildabilityChecker {
    pub(crate) async fn load(
        state: &Arc<ServerState>,
        shared_builds: &[MDerivationBuild],
    ) -> Result<Self> {
        let db = &state.worker_db;
        let drv_ids: Vec<DerivationId> = shared_builds.iter().map(|a| a.derivation).collect();

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
            features_by_drv,
            feature_name,
        })
    }

    /// A `Queued` shared build passed its gates, and a `Created` one is still behind its can-start
    /// counters. The graph writer already cleared `cache_available` once the miss budget was spent.
    /// Nothing is left to escalate here.
    pub(crate) fn any_buildable(
        &self,
        shared_builds: &[MDerivationBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> bool {
        shared_builds.iter().any(|a| {
            if a.status == BuildStatus::Building {
                return true;
            }
            if a.status != BuildStatus::Queued {
                return false;
            }
            self.drv_by_id
                .get(&a.derivation)
                .is_some_and(|drv| self.runnable(a, drv, worker_caps))
        })
    }

    fn runnable(
        &self,
        build: &MDerivationBuild,
        drv: &MDerivation,
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> bool {
        match decide_build_spec_kind(
            build.cache_available,
            &drv.architecture,
            drv.is_fixed_output,
        ) {
            BuildSpecKind::Substitute | BuildSpecKind::Download => !worker_caps.is_empty(),
            BuildSpecKind::Build => {
                let required = self.required_features_for(&build.derivation);
                worker_caps.iter().any(|(arch, feats)| {
                    let arch_ok = drv.architecture == gradient_types::BUILTIN_ARCH
                        || arch.iter().any(|a| a == &drv.architecture);
                    let feats_ok = required.iter().all(|f| feats.iter().any(|sf| sf == f));
                    arch_ok && feats_ok
                })
            }
        }
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

    pub(crate) fn unbuildable_imports(
        &self,
        imports: &[MDerivationBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> Vec<(DerivationBuildId, UnmetRequirement)> {
        imports
            .iter()
            .filter(|b| BuildStatus::PENDING.contains(&b.status))
            .filter_map(|b| {
                let drv = self.drv_by_id.get(&b.derivation)?;
                (!self.runnable(b, drv, worker_caps)).then(|| {
                    (
                        b.id,
                        UnmetRequirement {
                            architecture: drv.architecture.clone(),
                            required_features: self
                                .required_features_for(&b.derivation)
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                            build_count: 1,
                        },
                    )
                })
            })
            .collect()
    }

    pub(crate) fn compute_waiting_reason(
        &self,
        shared_builds: &[MDerivationBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> WaitingReason {
        let mut grouped: BTreeMap<(String, Vec<String>), u32> = BTreeMap::new();
        for a in shared_builds {
            let Some(drv) = self.drv_by_id.get(&a.derivation) else {
                continue;
            };
            if self.runnable(a, drv, worker_caps) {
                continue;
            }
            let required_owned: Vec<String> = self
                .required_features_for(&a.derivation)
                .into_iter()
                .map(str::to_owned)
                .collect();
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
            features_by_drv,
            feature_name,
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

    fn cache_available_build(drv_id: DerivationId, _eval_id: EvaluationId) -> MDerivationBuild {
        gradient_entity::derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: drv_id,
            status: BuildStatus::Queued,
            cache_available: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_passthrough_is_buildable_anywhere_whatever_its_architecture() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "aarch64-linux");
        let build = cache_available_build(d.id, eval_id);
        let checker = checker_with(vec![d], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let builds = [build];
        assert!(checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert!(unmet.is_empty());
    }

    #[test]
    fn without_a_connected_worker_nothing_is_substituted_or_downloaded() {
        let eval_id = EvaluationId::now_v7();
        let passthrough = drv(DerivationId::now_v7(), "x86_64-linux");
        let mut source = drv(DerivationId::now_v7(), gradient_types::BUILTIN_ARCH);
        source.is_fixed_output = true;
        let builds = [
            cache_available_build(passthrough.id, eval_id),
            build_for(source.id, eval_id),
        ];
        let checker = checker_with(vec![passthrough, source], vec![]);

        assert!(!checker.any_buildable(&builds, &[]));
        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, _, _) = workers_view(&reason);
        let mut systems: Vec<&str> = unmet.iter().map(|u| u.architecture.as_str()).collect();
        systems.sort_unstable();
        assert_eq!(systems, [gradient_types::BUILTIN_ARCH, "x86_64-linux"]);
    }

    #[test]
    fn an_exhausted_passthrough_is_an_ordinary_build_with_an_unmet_architecture() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "aarch64-linux");
        let mut build = cache_available_build(d.id, eval_id);
        build.cache_available = false;
        let checker = checker_with(vec![d], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let builds = [build];
        assert!(!checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
    }

    #[test]
    fn dependency_blocked_shared_build_is_not_buildable() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "x86_64-linux");
        let mut b = build_for(d.id, eval_id);
        b.status = BuildStatus::Created;
        let checker = checker_with(vec![d], vec![]);
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(!checker.any_buildable(&[b], &caps));
    }

    #[test]
    fn a_queued_real_build_is_buildable_when_a_worker_matches() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "x86_64-linux");
        let b = build_for(d.id, eval_id);
        let checker = checker_with(vec![d], vec![]);
        let shared_builds = std::slice::from_ref(&b);
        assert!(
            checker.any_buildable(shared_builds, &[(vec!["x86_64-linux".to_string()], vec![])])
        );
        assert!(!checker.any_buildable(
            shared_builds,
            &[(vec!["aarch64-linux".to_string()], vec![])]
        ));
    }

    #[test]
    fn a_pending_import_without_a_worker_for_its_system_is_unbuildable() {
        let pending = |drv_id, status| MDerivationBuild {
            status,
            ..build_for(drv_id, EvaluationId::now_v7())
        };
        let darwin = drv(DerivationId::now_v7(), "aarch64-darwin");
        let linux = drv(DerivationId::now_v7(), "x86_64-linux");
        let (stuck, building, buildable) = (
            pending(darwin.id, BuildStatus::Created),
            pending(darwin.id, BuildStatus::Building),
            pending(linux.id, BuildStatus::Queued),
        );
        let checker = checker_with(vec![darwin, linux], vec![]);
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];

        let unbuildable = checker.unbuildable_imports(&[stuck.clone(), building, buildable], &caps);

        assert_eq!(unbuildable.len(), 1, "{unbuildable:?}");
        assert_eq!(unbuildable[0].0, stuck.id);
        assert_eq!(unbuildable[0].1.architecture, "aarch64-darwin");
    }
}
