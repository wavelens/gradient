/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::{FlakeJob, FlakeStep};

#[derive(Debug, Clone, Default)]
pub struct WorkerCaps {
    /// The server is sending SSH credentials only to fetch-capable workers. Any `FetchFlake` step
    /// is therefore requiring this flag.
    pub fetch: bool,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub capabilities: gradient_wire::types::GradientCapabilities,
    pub metrics: Option<crate::score::WorkerMetricsView>,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
}

impl WorkerCaps {
    pub fn can_build(&self, architecture: &str, required_features: &[String]) -> bool {
        let arch_ok = architecture == gradient_types::BUILTIN_ARCH
            || self.architectures.iter().any(|a| a == architecture);
        let features_ok = required_features
            .iter()
            .all(|f| self.system_features.iter().any(|sf| sf == f));
        arch_ok && features_ok
    }

    pub fn can_eval(&self, job: &FlakeJob) -> bool {
        let needs_fetch = job.steps.contains(&FlakeStep::FetchFlake);
        let needs_eval = job
            .steps
            .iter()
            .any(|t| matches!(t, FlakeStep::EvaluateFlake | FlakeStep::EvaluateDerivations));

        (!needs_fetch || self.fetch) && (!needs_eval || self.capabilities.eval)
    }
}
