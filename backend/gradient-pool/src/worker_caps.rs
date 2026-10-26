/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::input::check_repository_url_is_ssh;
use gradient_wire::types::{BuildJob, BuildSpecKind, FlakeJob, FlakeSource, FlakeStep, JobKind};

pub fn build_needs_network(kind: BuildSpecKind, is_fixed_output: bool) -> bool {
    kind != BuildSpecKind::Build || is_fixed_output
}

#[derive(Debug, Clone, Default)]
pub struct WorkerCaps {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub capabilities: gradient_wire::types::GradientCapabilities,
    pub metrics: Option<crate::score::WorkerMetricsView>,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
}

impl WorkerCaps {
    pub fn takes_jobs_of(&self, kind: &JobKind) -> bool {
        match kind {
            JobKind::Flake => self.capabilities.eval || self.capabilities.network,
            JobKind::Build => self.capabilities.build,
        }
    }

    pub fn can_build(&self, architecture: &str, required_features: &[String]) -> bool {
        let arch_ok = architecture == gradient_types::BUILTIN_ARCH
            || self.architectures.iter().any(|a| a == architecture);
        let features_ok = required_features
            .iter()
            .all(|f| self.system_features.iter().any(|sf| sf == f));
        self.takes_jobs_of(&JobKind::Build) && arch_ok && features_ok
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::GradientCapabilities;

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
}
