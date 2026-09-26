/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::{FlakeJob, FlakeStep};

/// A connected worker's capabilities, used to gate which jobs are eligible
/// for assignment: the `fetch` gradient capability plus the Nix architectures
/// and system features it can build for.
#[derive(Debug, Clone, Default)]
pub struct WorkerCaps {
    /// Worker can fetch flake sources from a repository. Required for any
    /// FlakeJob carrying a `FetchFlake` step, since the server only sends SSH
    /// credentials to fetch-capable workers.
    pub fetch: bool,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    /// Full set of advertised gradient capabilities, surfaced on the dispatch view.
    pub capabilities: gradient_wire::types::GradientCapabilities,
    /// Live resource view of the worker, fed into resource-aware scoring rules.
    pub metrics: Option<crate::score::WorkerMetricsView>,
}

impl WorkerCaps {
    /// Returns true when this worker can execute a build with the given
    /// `architecture` and `required_features`. `"builtin"` derivations
    /// (`builtin:fetchurl` etc.) run on any architecture.
    pub fn can_build(&self, architecture: &str, required_features: &[String]) -> bool {
        let arch_ok = architecture == gradient_types::BUILTIN_ARCH
            || self.architectures.iter().any(|a| a == architecture);
        let features_ok = required_features
            .iter()
            .all(|f| self.system_features.iter().any(|sf| sf == f));
        arch_ok && features_ok
    }

    /// Returns true when this worker can run flake `job`: a `FetchFlake` step
    /// needs `fetch` (the server only sends fetch credentials to fetch workers)
    /// and any `EvaluateFlake`/`EvaluateDerivations` step needs `eval`.
    pub fn can_eval(&self, job: &FlakeJob) -> bool {
        let needs_fetch = job.steps.contains(&FlakeStep::FetchFlake);
        let needs_eval = job
            .steps
            .iter()
            .any(|t| matches!(t, FlakeStep::EvaluateFlake | FlakeStep::EvaluateDerivations));

        (!needs_fetch || self.fetch) && (!needs_eval || self.capabilities.eval)
    }
}
