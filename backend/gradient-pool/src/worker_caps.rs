/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::input::check_repository_url_is_ssh;
use std::collections::BTreeMap;

use gradient_wire::types::{
    BuildJob, BuildSpecKind, DeviceClass, FlakeJob, FlakeSource, FlakeStep, JobKind,
    RunnerInventory, RunnerRequirement,
};

pub fn build_needs_network(kind: BuildSpecKind, is_fixed_output: bool) -> bool {
    kind != BuildSpecKind::Build || is_fixed_output
}

pub fn effective_free(reported: &DeviceClass, held: u32) -> u32 {
    reported.free.min(reported.total.saturating_sub(held))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceCount {
    Free,
    Total,
}

impl DeviceCount {
    fn of(self, class: &DeviceClass) -> u32 {
        match self {
            Self::Free => class.free,
            Self::Total => class.total,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkerCaps {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub capabilities: gradient_wire::types::GradientCapabilities,
    pub metrics: Option<crate::score::WorkerMetricsView>,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
    pub runner: Option<RunnerInventory>,
}

impl WorkerCaps {
    pub fn takes_jobs_of(&self, kind: &JobKind) -> bool {
        match kind {
            JobKind::Flake => self.capabilities.eval || self.capabilities.network,
            JobKind::Build => self.capabilities.build,
            JobKind::Runner => self.capabilities.exec,
        }
    }

    fn runs_architecture(&self, architecture: &str) -> bool {
        architecture == gradient_types::BUILTIN_ARCH
            || self.architectures.iter().any(|a| a == architecture)
    }

    pub fn can_build(&self, architecture: &str, required_features: &[String]) -> bool {
        let features_ok = required_features
            .iter()
            .all(|f| self.system_features.iter().any(|sf| sf == f));
        self.takes_jobs_of(&JobKind::Build) && self.runs_architecture(architecture) && features_ok
    }

    pub fn can_run_runner(&self, req: &RunnerRequirement) -> bool {
        self.fits_runner(req, DeviceCount::Free)
    }

    pub fn fits_runner(&self, req: &RunnerRequirement, devices: DeviceCount) -> bool {
        let Some(inventory) = self.runner.as_ref() else {
            return false;
        };
        let allow = &inventory.allow;
        let (lowest_port, highest_port) = allow.ports;
        self.takes_jobs_of(&JobKind::Runner)
            && self.runs_architecture(&req.architecture)
            && req.labels.iter().all(|l| inventory.labels.contains(l))
            && devices_fit(req, inventory, devices)
            && req
                .mounts
                .iter()
                .all(|m| inventory.mounts.contains(m) && allow.mounts.contains(m))
            && (!req.network || allow.network)
            && (!req.ssh || allow.ssh)
            && req
                .ports
                .iter()
                .all(|p| (lowest_port..=highest_port).contains(p))
            && (!req.custom_image || allow.custom_images)
            && allow.nodes >= 1
    }

    pub fn can_run_build(&self, job: &BuildJob) -> bool {
        let needs_network = build_needs_network(job.build.kind, job.build.is_fixed_output);
        let only_transfers = job.build.kind != BuildSpecKind::Build;
        let runs_here = only_transfers
            || self.can_build(
                &job.requirement.architecture,
                &job.requirement.required_features,
            );
        runs_here && (!needs_network || self.capabilities.network)
    }

    pub fn can_eval(&self, job: &FlakeJob) -> bool {
        let needs_eval = job
            .steps
            .iter()
            .any(|t| matches!(t, FlakeStep::EvaluateFlake | FlakeStep::EvaluateDerivations));

        (!job.steps.contains(&FlakeStep::FetchFlake) || self.can_fetch(&job.source))
            && (!needs_eval || self.capabilities.eval)
    }

    pub fn can_fetch(&self, source: &FlakeSource) -> bool {
        let needs_secrets = matches!(source, FlakeSource::Repository { url, .. } if check_repository_url_is_ssh(url));
        self.can_fetch_input(needs_secrets)
    }

    pub fn can_fetch_input(&self, needs_secrets: bool) -> bool {
        self.capabilities.network && (!needs_secrets || self.capabilities.secrets)
    }
}

fn devices_fit(req: &RunnerRequirement, inventory: &RunnerInventory, devices: DeviceCount) -> bool {
    let mut wanted: BTreeMap<&str, u32> = BTreeMap::new();
    for device in &req.devices {
        *wanted.entry(device.name.as_str()).or_default() += device.count;
    }
    wanted.into_iter().all(|(name, count)| {
        inventory.allow.devices.iter().any(|d| d == name)
            && inventory
                .devices
                .iter()
                .any(|class| class.name == name && devices.of(class) >= count)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::{
        DeviceClass, DeviceRequest, GradientCapabilities, RunnerAllow, RunnerInventory,
        RunnerRequirement,
    };

    fn x86_worker(build: bool) -> WorkerCaps {
        WorkerCaps {
            architectures: vec!["x86_64-linux".into()],
            capabilities: GradientCapabilities {
                eval: true,
                build,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_worker_without_build_builds_no_job() {
        let eval_only = x86_worker(false);
        assert!(!eval_only.can_build(gradient_types::BUILTIN_ARCH, &[]));
        assert!(!eval_only.can_build("x86_64-linux", &[]));

        let builder = x86_worker(true);
        assert!(builder.can_build(gradient_types::BUILTIN_ARCH, &[]));
        assert!(builder.can_build("x86_64-linux", &[]));
    }

    fn fixed_output_job() -> BuildJob {
        job_of(BuildSpecKind::Build)
    }

    fn job_of(kind: BuildSpecKind) -> BuildJob {
        BuildJob {
            build: gradient_wire::types::BuildSpec {
                build_id: "b1".into(),
                drv_path: String::new(),
                kind,
                is_fixed_output: true,
                outputs: Vec::new(),
                timeout_secs: None,
                max_silent_secs: None,
                build_cores: None,
                input_realisations: Vec::new(),
            },
            requirement: gradient_wire::types::BuildRequirement {
                architecture: "x86_64-linux".into(),
                required_features: Vec::new(),
            },
        }
    }

    #[test]
    fn a_fixed_output_build_needs_network_and_build() {
        let with = |network: bool, build: bool| WorkerCaps {
            architectures: vec!["x86_64-linux".into()],
            capabilities: GradientCapabilities {
                network,
                build,
                ..Default::default()
            },
            ..Default::default()
        };
        let job = fixed_output_job();
        assert!(with(true, true).can_run_build(&job));
        assert!(
            !with(true, false).can_run_build(&job),
            "network without build"
        );
        assert!(
            !with(false, true).can_run_build(&job),
            "build without network"
        );
    }

    #[test]
    fn a_substitute_or_download_needs_only_network() {
        let network_only = WorkerCaps {
            capabilities: GradientCapabilities {
                network: true,
                ..Default::default()
            },
            ..Default::default()
        };
        for kind in [BuildSpecKind::Substitute, BuildSpecKind::Download] {
            assert!(network_only.can_run_build(&job_of(kind)), "{kind:?}");
            assert!(
                !WorkerCaps::default().can_run_build(&job_of(kind)),
                "{kind:?} without network"
            );
        }
    }

    fn repository(url: &str) -> FlakeSource {
        FlakeSource::Repository {
            url: url.into(),
            commit: "abc123".into(),
        }
    }

    #[test]
    fn an_ssh_repository_needs_secrets_and_a_public_one_only_network() {
        let network_only = WorkerCaps {
            capabilities: GradientCapabilities {
                network: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(network_only.can_fetch(&repository("https://example.com/repo")));
        assert!(!network_only.can_fetch(&repository("git+ssh://git@example.com/repo")));
        assert!(!WorkerCaps::default().can_fetch(&repository("https://example.com/repo")));
    }

    fn runner_worker() -> WorkerCaps {
        let device = |name: &str, total: u32| DeviceClass {
            name: name.into(),
            total,
            free: total,
        };
        WorkerCaps {
            architectures: vec!["x86_64-linux".into()],
            capabilities: GradientCapabilities {
                exec: true,
                ..Default::default()
            },
            runner: Some(RunnerInventory {
                labels: vec!["w1".into(), "lab-a".into()],
                devices: vec![device("gpu:h100", 8), device("kvm", 1)],
                mounts: vec!["datasets".into(), "scratch".into()],
                allow: RunnerAllow {
                    network: true,
                    ssh: false,
                    ports: (1024, 65535),
                    nodes: 8,
                    devices: vec!["gpu:h100".into(), "kvm".into()],
                    mounts: vec!["datasets".into(), "scratch".into()],
                    custom_images: false,
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn req(edit: impl FnOnce(&mut RunnerRequirement)) -> RunnerRequirement {
        let mut requirement = RunnerRequirement {
            architecture: "x86_64-linux".into(),
            ..Default::default()
        };
        edit(&mut requirement);
        requirement
    }

    fn gpus(count: u32) -> Vec<DeviceRequest> {
        vec![DeviceRequest {
            name: "gpu:h100".into(),
            count,
        }]
    }

    #[test]
    fn every_label_must_be_present_and_the_worker_id_is_one() {
        let w = runner_worker();
        assert!(w.can_run_runner(&req(|r| r.labels = vec!["w1".into()])));
        assert!(w.can_run_runner(&req(|r| r.labels = vec!["lab-a".into(), "w1".into()])));
        assert!(!w.can_run_runner(&req(|r| r.labels = vec!["3d-printer".into()])));
    }

    #[test]
    fn devices_need_enough_free_units_and_an_allowed_name() {
        let w = runner_worker();
        assert!(w.can_run_runner(&req(|r| r.devices = gpus(8))));
        assert!(!w.can_run_runner(&req(|r| r.devices = gpus(9))));
        assert!(!w.can_run_runner(&req(|r| r.devices = [gpus(4), gpus(5)].concat())));
        let camera = vec![DeviceRequest {
            name: "camera".into(),
            count: 1,
        }];
        assert!(!w.can_run_runner(&req(|r| r.devices = camera)));
    }

    #[test]
    fn busy_devices_fit_against_the_total_only() {
        let mut w = runner_worker();
        if let Some(inventory) = w.runner.as_mut() {
            inventory.devices[0].free = 0;
        }
        assert!(!w.fits_runner(&req(|r| r.devices = gpus(8)), DeviceCount::Free));
        assert!(w.fits_runner(&req(|r| r.devices = gpus(8)), DeviceCount::Total));
    }

    #[test]
    fn mounts_network_ssh_and_ports_stay_inside_allow() {
        let w = runner_worker();
        assert!(w.can_run_runner(&req(|r| r.mounts = vec!["datasets".into()])));
        assert!(!w.can_run_runner(&req(|r| r.mounts = vec!["checkpoints".into()])));
        assert!(w.can_run_runner(&req(|r| r.network = true)));
        assert!(
            !w.can_run_runner(&req(|r| r.ssh = true)),
            "allow beats the request"
        );
        assert!(!w.can_run_runner(&req(|r| r.ports = vec![80])));
        assert!(w.can_run_runner(&req(|r| r.ports = vec![6006])));
    }

    #[test]
    fn a_custom_image_needs_custom_images_and_a_worker_without_exec_runs_nothing() {
        assert!(!runner_worker().can_run_runner(&req(|r| r.custom_image = true)));
        let mut no_exec = runner_worker();
        no_exec.capabilities.exec = false;
        assert!(!no_exec.can_run_runner(&req(|_| {})));
        let no_inventory = WorkerCaps {
            runner: None,
            ..runner_worker()
        };
        assert!(!no_inventory.can_run_runner(&req(|_| {})));
    }
}
