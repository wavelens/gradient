/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::VecDeque;
use std::sync::Arc;

use gradient_util::sync::Mutex;
use gradient_wire::types::BuildMetrics;

const MAX_LOCAL_BUILT_NOT_UPLOADED_DERIVATION_ENTRIES: usize = 256;

type BuiltDerivationsAwaitingOutputUploadWithBuildMetrics =
    VecDeque<(String, Option<BuildMetrics>)>;

/// Derivations from builds by this worker itself, with outputs that still need an upload. Retry
/// jobs for such derivations always report those outputs as built (substituted=false), not
/// substituted. Retry jobs only add metrics from the real build, if the first report never
/// reached the server. Restarts of the worker process clear this memory. Outputs that are already
/// in the store count as substituted again after a restart.
#[derive(Clone, Default)]
pub(crate) struct UnreportedBuildMetricsByDerivationPath {
    entries: Arc<Mutex<BuiltDerivationsAwaitingOutputUploadWithBuildMetrics>>,
}

impl UnreportedBuildMetricsByDerivationPath {
    pub(crate) fn record_built_derivation(&self, drv_path: &str, metrics: Option<BuildMetrics>) {
        let mut entries = self.entries.lock();
        entries.retain(|(path, _)| path != drv_path);
        entries.push_back((drv_path.to_owned(), metrics));
        while entries.len() > MAX_LOCAL_BUILT_NOT_UPLOADED_DERIVATION_ENTRIES {
            entries.pop_front();
        }
    }

    pub(crate) fn report_for_outputs_already_in_store(
        &self,
        drv_path: &str,
    ) -> (Option<BuildMetrics>, bool) {
        self.entries
            .lock()
            .iter_mut()
            .find(|(path, _)| path == drv_path)
            .map_or((None, true), |(_, metrics)| (metrics.take(), false))
    }

    pub(crate) fn remove_derivation_with_uploaded_outputs(&self, drv_path: &str) {
        self.entries.lock().retain(|(path, _)| path != drv_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRV: &str = "/nix/store/x.drv";

    fn metrics() -> BuildMetrics {
        BuildMetrics {
            build_time_ms: Some(600_000),
            ..BuildMetrics::default()
        }
    }

    #[test]
    fn test_built_derivation_reported_as_built_with_remembered_metrics() {
        let memory = UnreportedBuildMetricsByDerivationPath::default();
        memory.record_built_derivation(DRV, Some(metrics()));

        assert_eq!(
            memory.report_for_outputs_already_in_store(DRV),
            (Some(metrics()), false)
        );
        assert_eq!(
            memory.report_for_outputs_already_in_store(DRV),
            (None, false)
        );
    }

    #[test]
    fn test_outputs_in_store_for_derivation_not_built_by_worker_stay_substituted_without_metrics() {
        let memory = UnreportedBuildMetricsByDerivationPath::default();
        memory.record_built_derivation("/nix/store/other.drv", Some(metrics()));

        assert_eq!(
            memory.report_for_outputs_already_in_store(DRV),
            (None, true)
        );
    }

    #[test]
    fn built_derivation_is_reported_once_and_after_output_upload_is_forgotten_and_reported_substituted()
     {
        let memory = UnreportedBuildMetricsByDerivationPath::default();
        memory.record_built_derivation(DRV, Some(metrics()));
        memory.record_built_derivation(DRV, None);

        assert_eq!(
            memory.report_for_outputs_already_in_store(DRV),
            (None, false)
        );

        memory.remove_derivation_with_uploaded_outputs(DRV);

        assert_eq!(
            memory.report_for_outputs_already_in_store(DRV),
            (None, true)
        );
    }

    #[test]
    fn drops_oldest_entry_when_more_than_fixed_maximum_entries() {
        let memory = UnreportedBuildMetricsByDerivationPath::default();
        for n in 0..=MAX_LOCAL_BUILT_NOT_UPLOADED_DERIVATION_ENTRIES {
            memory.record_built_derivation(&format!("/nix/store/{n}.drv"), None);
        }

        assert_eq!(
            memory.entries.lock().len(),
            MAX_LOCAL_BUILT_NOT_UPLOADED_DERIVATION_ENTRIES
        );
        assert!(
            memory
                .report_for_outputs_already_in_store("/nix/store/0.drv")
                .1
        );
        assert!(
            !memory
                .report_for_outputs_already_in_store("/nix/store/1.drv")
                .1
        );
    }
}
