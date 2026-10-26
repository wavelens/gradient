/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{ClusterJobId, EvaluationId};
use gradient_graph::{NarCommit, UpstreamHit};
use gradient_pool::WorkerProfile;
use gradient_types::runner::RunnerInventory;
use gradient_wire::types::{
    BuildOutput, DerivationOutput, DiscoveredDerivation, GradientCapabilities, JobPhase,
    JobPhaseSpan,
};

use gradient_fixtures::shape::Shape;

pub(crate) const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
pub(crate) const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";
pub(crate) const C: &str = "cccccccccccccccccccccccccccccccc-c.drv";
pub(crate) const D: &str = "dddddddddddddddddddddddddddddddd-d.drv";
pub(crate) const N: &str = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn-n.drv";
pub(crate) const P: &str = "pppppppppppppppppppppppppppppppp-p.drv";
pub(crate) const S: &str = "ssssssssssssssssssssssssssssssss-s.drv";
pub(crate) const X: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-x.drv";
pub(crate) const Y: &str = "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy-y.drv";
pub(crate) const A1: &str = "11111111111111111111111111111111-a1.drv";
pub(crate) const A2: &str = "22222222222222222222222222222222-a2.drv";
pub(crate) const A3: &str = "33333333333333333333333333333333-a3.drv";

pub(crate) const A_OUT: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
pub(crate) const B_OUT: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";
pub(crate) const C_OUT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
pub(crate) const D_OUT: &str = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";
pub(crate) const N_OUT: &str = "n0n0n0n0n0n0n0n0n0n0n0n0n0n0n0n0";
pub(crate) const P_OUT: &str = "p0p0p0p0p0p0p0p0p0p0p0p0p0p0p0p0";
pub(crate) const S_OUT: &str = "s0s0s0s0s0s0s0s0s0s0s0s0s0s0s0s0";
pub(crate) const X_OUT: &str = "x0x0x0x0x0x0x0x0x0x0x0x0x0x0x0x0";
pub(crate) const Y_OUT: &str = "y0y0y0y0y0y0y0y0y0y0y0y0y0y0y0y0";
pub(crate) const A1_OUT: &str = "10101010101010101010101010101010";
pub(crate) const A2_OUT: &str = "20202020202020202020202020202020";
pub(crate) const A3_OUT: &str = "30303030303030303030303030303030";
pub(crate) const A_DEV: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

pub(crate) const REPOSITORY: &str = "https://example.invalid/repo.git";
pub(crate) const X86: &str = "x86_64-linux";
pub(crate) const ARM: &str = "aarch64-linux";

#[derive(Clone, Debug)]
pub(crate) struct Profile {
    pub caps: GradientCapabilities,
    pub hardware: Option<WorkerProfile>,
}

impl Profile {
    fn hardware(mut self, change: impl FnOnce(&mut WorkerProfile)) -> Self {
        let mut hardware = self.hardware.unwrap_or_else(|| WorkerProfile {
            architectures: vec![X86.into()],
            max_concurrent_builds: 1,
            ..WorkerProfile::default()
        });
        change(&mut hardware);
        self.hardware = Some(hardware);
        self
    }

    pub fn slots(self, slots: u32) -> Self {
        self.hardware(|h| h.max_concurrent_builds = slots)
    }

    pub fn arch(self, architectures: &[&str]) -> Self {
        self.hardware(|h| h.architectures = architectures.iter().map(|a| (*a).into()).collect())
    }

    pub fn features(self, features: &[&str]) -> Self {
        self.hardware(|h| h.system_features = features.iter().map(|f| (*f).into()).collect())
    }

    pub fn ram_total_mb(self, ram_total_mb: u64) -> Self {
        self.hardware(|h| h.ram_total_mb = ram_total_mb)
    }

    pub fn cpus(self, cpu_count: u32) -> Self {
        self.hardware(|h| h.cpu_count = cpu_count)
    }

    pub fn core_limit(self, build_cores: u32) -> Self {
        self.hardware(|h| h.build_cores = Some(build_cores))
    }

    pub fn without_profile(mut self) -> Self {
        self.hardware = None;
        self
    }

    pub fn runner(mut self, inventory: RunnerInventory) -> Self {
        self.caps.exec = true;
        self.hardware(|h| h.runner = Some(inventory))
    }
}

pub(crate) fn w_runner(inventory: RunnerInventory) -> Profile {
    Profile {
        caps: GradientCapabilities::default(),
        hardware: None,
    }
    .runner(inventory)
}

fn caps(network: bool, secrets: bool, eval: bool, build: bool) -> GradientCapabilities {
    GradientCapabilities {
        network,
        secrets,
        eval,
        build,
        ..GradientCapabilities::default()
    }
}

pub(crate) fn w_all() -> Profile {
    Profile {
        caps: caps(true, true, true, true),
        hardware: None,
    }
    .slots(1)
}

pub(crate) fn w_build() -> Profile {
    Profile {
        caps: caps(false, false, false, true),
        hardware: None,
    }
    .slots(1)
}

pub(crate) fn w_network_build() -> Profile {
    Profile {
        caps: caps(true, false, false, true),
        hardware: None,
    }
    .slots(1)
}

pub(crate) fn w_eval() -> Profile {
    Profile {
        caps: caps(false, false, true, false),
        hardware: None,
    }
}

pub(crate) fn w_network_eval() -> Profile {
    Profile {
        caps: caps(true, false, true, false),
        hardware: None,
    }
}

pub(crate) fn w_network() -> Profile {
    Profile {
        caps: caps(true, false, false, false),
        hardware: None,
    }
}

pub(crate) fn w_network_secrets() -> Profile {
    Profile {
        caps: caps(true, true, false, false),
        hardware: None,
    }
}

pub(crate) fn eval_key(evaluation: EvaluationId) -> String {
    crate::jobs::eval_job_key(evaluation, 0)
}

pub(crate) fn ago(secs: i64) -> NaiveDateTime {
    gradient_types::now() - chrono::Duration::seconds(secs)
}

pub(crate) fn from_now(secs: i64) -> NaiveDateTime {
    gradient_types::now() + chrono::Duration::seconds(secs)
}

pub(crate) fn is_recent(at: NaiveDateTime) -> bool {
    (gradient_types::now() - at).num_seconds().abs() <= 60
}

pub(crate) fn attr_of(path: &str) -> String {
    format!("pkg.{}", package_of(path).trim_end_matches(".drv"))
}

fn package_of(path: &str) -> &str {
    path.split_once('-').map_or(path, |(_, package)| package)
}

pub(crate) trait CaseShapes {
    fn startable(self, path: &str, out: &str) -> Self;
    fn one(self, evaluation: EvaluationId, path: &str, out: &str) -> Self;
    fn two(self, evaluation: EvaluationId) -> Self;
    fn chain(self, evaluation: EvaluationId) -> Self;
    fn shared(self, first: EvaluationId, second: EvaluationId) -> Self;
    fn passthrough(self, evaluation: EvaluationId) -> Self;
    fn evalq(self, evaluation: EvaluationId) -> Self;
    fn cl2(
        self,
        cluster: ClusterJobId,
        member_evaluation: EvaluationId,
        build_evaluation: EvaluationId,
        path: &str,
        out: &str,
    ) -> Self;
}

impl CaseShapes for Shape {
    fn startable(self, path: &str, out: &str) -> Self {
        self.derivation(path)
            .walked()
            .probed()
            .drv_cached()
            .entry_point(&attr_of(path))
            .job()
            .output(out)
            .build_status(BuildStatus::Queued)
    }

    fn one(self, evaluation: EvaluationId, path: &str, out: &str) -> Self {
        self.evaluation(evaluation)
            .evaluation_status(EvaluationStatus::Building)
            .startable(path, out)
    }

    fn two(self, evaluation: EvaluationId) -> Self {
        self.one(evaluation, A, A_OUT).startable(B, B_OUT)
    }

    fn chain(self, evaluation: EvaluationId) -> Self {
        self.evaluation(evaluation)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .probed()
            .drv_cached()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .job()
            .output(B_OUT)
            .build_status(BuildStatus::Queued)
    }

    fn shared(self, first: EvaluationId, second: EvaluationId) -> Self {
        self.one(first, A, A_OUT)
            .evaluation(second)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .entry_point(&attr_of(A))
            .job()
    }

    fn passthrough(self, evaluation: EvaluationId) -> Self {
        self.evaluation(evaluation)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .probed()
            .cache_available()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT)
            .build_status(BuildStatus::Queued)
    }

    fn evalq(self, evaluation: EvaluationId) -> Self {
        self.evaluation(evaluation)
            .evaluation_status(EvaluationStatus::Queued)
            .repository(REPOSITORY)
    }

    fn cl2(
        self,
        cluster: ClusterJobId,
        member_evaluation: EvaluationId,
        build_evaluation: EvaluationId,
        path: &str,
        out: &str,
    ) -> Self {
        self.evalq(member_evaluation)
            .evaluation_member_of(cluster, "head", true, None)
            .one(build_evaluation, path, out)
            .build_member_of(cluster, "node", false, None)
            .cluster(
                cluster,
                false,
                2,
                ClusterJobStatus::Queued,
                0,
                gradient_types::now(),
            )
    }
}

pub(crate) fn span(phase: JobPhase, start_ms: u64, end_ms: u64) -> JobPhaseSpan {
    JobPhaseSpan {
        phase,
        start_ms,
        end_ms,
        parent: None,
        paths: 0,
        bytes: 0,
    }
}

pub(crate) fn store_path(hash: &str, package: &str) -> String {
    format!("/nix/store/{hash}-{package}")
}

pub(crate) fn drv_path(path: &str) -> String {
    format!("/nix/store/{path}")
}

pub(crate) fn drv(path: &str, dependencies: &[&str]) -> DiscoveredDerivation {
    DiscoveredDerivation {
        drv_path: drv_path(path),
        dependencies: dependencies.iter().map(|d| drv_path(d)).collect(),
        architecture: X86.to_owned(),
        allow_substitutes: true,
        ..Default::default()
    }
}

pub(crate) fn with_output(
    mut derivation: DiscoveredDerivation,
    hash: &str,
) -> DiscoveredDerivation {
    let package = package_of(&derivation.drv_path)
        .trim_end_matches(".drv")
        .to_owned();
    derivation.outputs.push(DerivationOutput {
        name: "out".into(),
        path: store_path(hash, &package),
    });
    derivation
}

pub(crate) fn with_attr(mut derivation: DiscoveredDerivation, attr: &str) -> DiscoveredDerivation {
    derivation.attr = attr.to_owned();
    derivation
}

pub(crate) fn with_ifd(mut derivation: DiscoveredDerivation) -> DiscoveredDerivation {
    derivation.ifd = true;
    derivation
}

pub(crate) fn nar(hash: &str, package: &str) -> NarCommit {
    NarCommit {
        store_path: store_path(hash, package),
        file_hash: "sha256:abc".into(),
        file_size: 5,
        nar_size: 5,
        nar_hash: "sha256:def".into(),
        confirmed: true,
        ..Default::default()
    }
}

pub(crate) fn drv_nar(path: &str) -> NarCommit {
    let (hash, package) = path
        .split_once('-')
        .unwrap_or_else(|| panic!("{path} is no drv path"));
    nar(hash, package)
}

pub(crate) fn output_nar(path: &str, hash: &str) -> NarCommit {
    nar(hash, package_of(path).trim_end_matches(".drv"))
}

pub(crate) fn with_references(mut commit: NarCommit, paths: &[(&str, &str)]) -> NarCommit {
    commit.references = paths
        .iter()
        .map(|(hash, package)| format!("{hash}-{package}"))
        .collect();
    commit
}

pub(crate) fn narinfo(references: &[(&str, &str)]) -> UpstreamHit {
    UpstreamHit {
        url: Some("https://upstream.invalid/nar/x.nar".into()),
        nar_hash: Some("sha256:def".into()),
        file_hash: Some("sha256:abc".into()),
        file_size: Some(5),
        nar_size: Some(5),
        references: Some(
            references
                .iter()
                .map(|(hash, package)| format!("{hash}-{package}"))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        ..Default::default()
    }
}

pub(crate) fn build_output(path: &str, hash: &str) -> BuildOutput {
    let package = package_of(path).trim_end_matches(".drv");
    BuildOutput {
        name: "out".into(),
        store_path: store_path(hash, package),
        hash: hash.into(),
        nar_size: Some(5),
        nar_hash: Some("sha256:def".into()),
        products: Vec::new(),
    }
}

pub(crate) fn polling(interval_secs: u32, branch: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "interval_secs": interval_secs, "branch": branch })
}

pub(crate) fn time_trigger(cron: &str) -> serde_json::Value {
    serde_json::json!({ "cron": cron })
}
