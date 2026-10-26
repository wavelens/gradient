/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::by_ids;
use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};

use gradient_db::WorkerDb;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::assign_mode::decide_build_spec_kind;
use crate::count::clamp_to_u32;
use gradient_pool::{DeviceCount, WorkerCaps, build_needs_network};
use gradient_types::runner::{DeviceRequest, RunnerInventory, RunnerSpec};
use gradient_types::waiting_reason::{EvalCapability, RunnerUnmet};
use gradient_wire::types::{BuildSpecKind, JobKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildWorkerCaps {
    pub architectures: Vec<String>,
    pub features: Vec<String>,
    pub network: bool,
    pub build: bool,
}

impl From<&gradient_pool::WorkerInfo> for BuildWorkerCaps {
    fn from(worker: &gradient_pool::WorkerInfo) -> Self {
        Self {
            architectures: worker.architectures.clone(),
            features: worker.system_features.clone(),
            network: worker.capabilities.network,
            build: worker.capabilities.build,
        }
    }
}

pub(crate) struct BuildabilityChecker {
    drv_by_id: HashMap<DerivationId, MDerivation>,
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_name: HashMap<FeatureId, String>,
}

impl BuildabilityChecker {
    pub(crate) async fn load(db: &WorkerDb, shared_builds: &[MDerivationBuild]) -> Result<Self> {
        let drv_ids: Vec<DerivationId> = shared_builds.iter().map(|a| a.derivation).collect();

        let drv_by_id: HashMap<DerivationId, MDerivation> = by_ids::<EDerivation, _>(db, &drv_ids)
            .await
            .context("fetch derivations for pending builds")?;

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
        let feature_name: HashMap<FeatureId, String> = by_ids::<EFeature, _>(db, &feature_ids)
            .await
            .context("fetch feature names")?
            .into_iter()
            .map(|(id, feature)| (id, feature.name))
            .collect();

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
        worker_caps: &[BuildWorkerCaps],
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
        worker_caps: &[BuildWorkerCaps],
    ) -> bool {
        let kind = decide_build_spec_kind(
            build.cache_available,
            &drv.architecture,
            drv.is_fixed_output,
        );
        let needs_network = build_needs_network(kind, drv.is_fixed_output);
        let mut able = worker_caps.iter().filter(|w| w.network || !needs_network);
        match kind {
            BuildSpecKind::Substitute | BuildSpecKind::Download => able.next().is_some(),
            BuildSpecKind::Build => {
                let required = self.required_features_for(&build.derivation);
                able.filter(|w| w.build).any(|w| {
                    let arch_ok = drv.architecture == gradient_types::BUILTIN_ARCH
                        || w.architectures.iter().any(|a| a == &drv.architecture);
                    let feats_ok = required.iter().all(|f| w.features.iter().any(|sf| sf == f));
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
        worker_caps: &[BuildWorkerCaps],
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
        worker_caps: &[BuildWorkerCaps],
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
            .filter(|w| w.build)
            .flat_map(|w| w.architectures.iter().cloned())
            .collect();
        available_architectures.sort_unstable();
        available_architectures.dedup();

        WaitingReason::Workers {
            unmet,
            connected_workers: clamp_to_u32(worker_caps.len()),
            available_architectures,
        }
    }
}

pub(crate) fn runner_waiting_reason(
    spec: &RunnerSpec,
    architecture: &str,
    workers: &[WorkerCaps],
) -> Option<WaitingReason> {
    let exec_workers: Vec<&WorkerCaps> = workers.iter().filter(|w| runs_runners(w)).collect();
    if exec_workers.is_empty() {
        return Some(WaitingReason::eval_workers(
            EvalCapability::Exec,
            clamp_to_u32(workers.len()),
        ));
    }

    let fits = |worker: &WorkerCaps, devices: DeviceCount| {
        let defaults = worker
            .runner
            .as_ref()
            .map(|inventory| inventory.defaults.clone())
            .unwrap_or_default();
        worker.fits_runner(&spec.requirement_on(architecture, &defaults), devices)
    };
    if exec_workers.iter().any(|w| fits(w, DeviceCount::Free)) {
        return None;
    }

    Some(WaitingReason::RunnerWorkers {
        unmet: runner_unmet(spec, architecture, workers),
        connected_workers: clamp_to_u32(workers.len()),
        matching_workers: clamp_to_u32(
            exec_workers
                .iter()
                .filter(|w| fits(w, DeviceCount::Total))
                .count(),
        ),
    })
}

pub(crate) fn runner_unmet(
    spec: &RunnerSpec,
    architecture: &str,
    workers: &[WorkerCaps],
) -> RunnerUnmet {
    let inventories: Vec<&RunnerInventory> = workers
        .iter()
        .filter(|w| runs_runners(w) && w.architectures.iter().any(|a| a == architecture))
        .filter_map(|w| w.runner.as_ref())
        .collect();
    let offered = |offers: &dyn Fn(&RunnerInventory) -> bool| inventories.iter().any(|i| offers(i));

    RunnerUnmet {
        labels: spec
            .labels
            .iter()
            .filter(|label| !offered(&|i| i.labels.contains(label)))
            .cloned()
            .collect(),
        devices: spec
            .devices
            .iter()
            .filter(|device| !offered(&|i| offers_device(i, device)))
            .cloned()
            .collect(),
        mounts: spec
            .mounts
            .iter()
            .flatten()
            .filter(|mount| {
                !offered(&|i| i.mounts.contains(mount) && i.allow.mounts.contains(mount))
            })
            .cloned()
            .collect(),
        nodes: if offered(&|i| i.allow.nodes >= spec.nodes) {
            0
        } else {
            spec.nodes
        },
        custom_image: spec.image.is_some() && !offered(&|i| i.allow.custom_images),
    }
}

fn runs_runners(worker: &WorkerCaps) -> bool {
    worker.takes_jobs_of(&JobKind::Runner) && worker.runner.is_some()
}

fn offers_device(inventory: &RunnerInventory, device: &DeviceRequest) -> bool {
    inventory.allow.devices.contains(&device.name)
        && inventory
            .devices
            .iter()
            .any(|class| class.name == device.name && class.total >= device.count)
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

        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];
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

        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];
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

        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];
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

        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];
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

        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];
        let builds = [build];
        assert!(!checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
    }

    fn builder(architecture: &str, network: bool) -> BuildWorkerCaps {
        BuildWorkerCaps {
            architectures: vec![architecture.to_owned()],
            features: Vec::new(),
            network,
            build: true,
        }
    }

    fn network_only() -> BuildWorkerCaps {
        BuildWorkerCaps {
            build: false,
            ..builder("x86_64-linux", true)
        }
    }

    #[test]
    fn a_transfer_runs_on_a_network_worker_without_build() {
        let eval_id = EvaluationId::now_v7();
        let passthrough = drv(DerivationId::now_v7(), "aarch64-linux");
        let mut source = drv(DerivationId::now_v7(), gradient_types::BUILTIN_ARCH);
        source.is_fixed_output = true;
        let substitute = [cache_available_build(passthrough.id, eval_id)];
        let download = [build_for(source.id, eval_id)];
        let checker = checker_with(vec![passthrough, source], vec![]);

        assert!(checker.any_buildable(&substitute, &[network_only()]));
        assert!(checker.any_buildable(&download, &[network_only()]));
    }

    #[test]
    fn a_build_needs_a_build_worker() {
        let mut fod = drv(DerivationId::now_v7(), "x86_64-linux");
        fod.is_fixed_output = true;
        let plain = drv(DerivationId::now_v7(), "x86_64-linux");
        let builds = [
            build_for(fod.id, EvaluationId::now_v7()),
            build_for(plain.id, EvaluationId::now_v7()),
        ];
        let checker = checker_with(vec![fod, plain], vec![]);

        assert!(!checker.any_buildable(&builds, &[network_only()]));
        let reason = checker.compute_waiting_reason(&builds, &[network_only()]);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);
        assert_eq!(
            unmet.len(),
            1,
            "both builds still need a builder: {unmet:?}"
        );
        assert_eq!(connected_workers, 1);
        assert!(available_architectures.is_empty());
    }

    #[test]
    fn a_fixed_output_build_needs_a_network_worker() {
        let mut d = drv(DerivationId::now_v7(), "x86_64-linux");
        d.is_fixed_output = true;
        let builds = [build_for(d.id, EvaluationId::now_v7())];
        let checker = checker_with(vec![d], vec![]);

        assert!(!checker.any_buildable(&builds, &[builder("x86_64-linux", false)]));
        assert!(checker.any_buildable(&builds, &[builder("x86_64-linux", true)]));
    }

    #[test]
    fn dependency_blocked_shared_build_is_not_buildable() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "x86_64-linux");
        let mut b = build_for(d.id, eval_id);
        b.status = BuildStatus::Created;
        let checker = checker_with(vec![d], vec![]);
        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];
        assert!(!checker.any_buildable(&[b], &caps));
    }

    #[test]
    fn a_queued_real_build_is_buildable_when_a_worker_matches() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "x86_64-linux");
        let b = build_for(d.id, eval_id);
        let checker = checker_with(vec![d], vec![]);
        let shared_builds = std::slice::from_ref(&b);
        assert!(checker.any_buildable(shared_builds, &[builder("x86_64-linux", true)]));
        assert!(!checker.any_buildable(shared_builds, &[builder("aarch64-linux", true)]));
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
        let caps: Vec<BuildWorkerCaps> = vec![builder("x86_64-linux", true)];

        let unbuildable = checker.unbuildable_imports(&[stuck.clone(), building, buildable], &caps);

        assert_eq!(unbuildable.len(), 1, "{unbuildable:?}");
        assert_eq!(unbuildable[0].0, stuck.id);
        assert_eq!(unbuildable[0].1.architecture, "aarch64-darwin");
    }

    #[test]
    fn a_job_without_any_exec_worker_waits_for_exec() {
        use crate::test_jobs::build_only_caps;
        let reason =
            runner_waiting_reason(&RunnerSpec::default(), "x86_64-linux", &[build_only_caps()]);
        assert_eq!(
            reason,
            Some(WaitingReason::eval_workers(EvalCapability::Exec, 1))
        );
    }

    #[test]
    fn the_wait_reason_names_what_no_worker_offers() {
        use crate::test_jobs::eight_gpu_runner_caps;
        let spec = RunnerSpec {
            labels: vec!["3d-printer".into()],
            devices: vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 16,
            }],
            mounts: Some(vec!["datasets".into()]),
            ..RunnerSpec::default()
        };
        let Some(WaitingReason::RunnerWorkers {
            unmet,
            connected_workers,
            matching_workers,
        }) = runner_waiting_reason(&spec, "x86_64-linux", &[eight_gpu_runner_caps()])
        else {
            panic!("runner wait")
        };
        assert_eq!(unmet.labels, vec!["3d-printer".to_owned()]);
        assert_eq!(
            unmet.devices,
            vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 16
            }]
        );
        assert!(unmet.mounts.is_empty());
        assert_eq!((connected_workers, matching_workers), (1, 0));
    }

    #[test]
    fn busy_devices_count_as_matching_workers() {
        use crate::test_jobs::eight_gpu_runner_caps_with_free;
        let spec = RunnerSpec {
            devices: vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 8,
            }],
            ..RunnerSpec::default()
        };
        let Some(WaitingReason::RunnerWorkers {
            unmet,
            matching_workers,
            ..
        }) = runner_waiting_reason(&spec, "x86_64-linux", &[eight_gpu_runner_caps_with_free(0)])
        else {
            panic!("runner wait")
        };
        assert_eq!((unmet, matching_workers), (RunnerUnmet::default(), 1));
    }
}
