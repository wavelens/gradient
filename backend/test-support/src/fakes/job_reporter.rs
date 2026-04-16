/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recording [`JobReporter`] that captures all calls for test assertions.

use anyhow::Result;
use async_trait::async_trait;
use proto::messages::{BuildOutput, CachedPath, DiscoveredDerivation, FetchedInput, QueryMode};
use proto::traits::JobReporter;

/// A reported event captured by [`RecordingJobReporter`].
#[derive(Debug, Clone)]
pub enum ReportedEvent {
    Fetching,
    FetchResult {
        fetched_paths: Vec<FetchedInput>,
    },
    EvaluatingFlake,
    EvaluatingDerivations,
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    },
    Building {
        build_id: String,
    },
    BuildOutput {
        build_id: String,
        outputs: Vec<BuildOutput>,
    },
    Compressing,
    Signing,
    LogChunk {
        task_index: u32,
        data: Vec<u8>,
    },
}

/// [`JobReporter`] that records every call as a [`ReportedEvent`].
#[derive(Debug, Default)]
pub struct RecordingJobReporter {
    pub events: Vec<ReportedEvent>,
    /// Paths to return from `query_cache`. Tests set this to simulate
    /// paths already present in the server's cache.
    pub cached_paths: Vec<String>,
}

impl RecordingJobReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure paths that `query_cache` will report as cached.
    pub fn with_cached_paths(mut self, paths: Vec<String>) -> Self {
        self.cached_paths = paths;
        self
    }

    /// Number of events recorded.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Get the last `EvalResult` event, if any.
    pub fn last_eval_result(&self) -> Option<&ReportedEvent> {
        self.events
            .iter()
            .rev()
            .find(|e| matches!(e, ReportedEvent::EvalResult { .. }))
    }

    /// Collect all derivations across every `EvalResult` event (incremental batches).
    pub fn all_eval_derivations(&self) -> Vec<&DiscoveredDerivation> {
        self.events
            .iter()
            .filter_map(|e| {
                if let ReportedEvent::EvalResult { derivations, .. } = e {
                    Some(derivations.iter())
                } else {
                    None
                }
            })
            .flatten()
            .collect()
    }
}

#[async_trait]
impl JobReporter for RecordingJobReporter {
    async fn query_cache(&mut self, paths: Vec<String>, mode: QueryMode) -> Result<Vec<CachedPath>> {
        let cached_set: std::collections::HashSet<&str> =
            self.cached_paths.iter().map(|s| s.as_str()).collect();
        Ok(paths
            .into_iter()
            .filter_map(|path| {
                let is_cached = cached_set.contains(path.as_str());
                // Normal/Pull: return only cached paths.
                // Push: return all paths with cached flag.
                if is_cached || matches!(mode, QueryMode::Push) {
                    Some(CachedPath {
                        path,
                        cached: is_cached,
                        file_size: None,
                        nar_size: None,
                        url: None,
                        nar_hash: None,
                        references: None,
                        signatures: None,
                        deriver: None,
                        ca: None,
                    })
                } else {
                    None
                }
            })
            .collect())
    }

    async fn report_fetching(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::Fetching);
        Ok(())
    }

    async fn report_fetch_result(&mut self, fetched_paths: Vec<FetchedInput>) -> Result<()> {
        self.events
            .push(ReportedEvent::FetchResult { fetched_paths });
        Ok(())
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::EvaluatingFlake);
        Ok(())
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::EvaluatingDerivations);
        Ok(())
    }

    async fn report_eval_result(
        &mut self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        self.events.push(ReportedEvent::EvalResult {
            derivations,
            warnings,
            errors,
        });
        Ok(())
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.events.push(ReportedEvent::Building { build_id });
        Ok(())
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        self.events
            .push(ReportedEvent::BuildOutput { build_id, outputs });
        Ok(())
    }

    async fn report_compressing(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::Compressing);
        Ok(())
    }

    async fn report_signing(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::Signing);
        Ok(())
    }

    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.events
            .push(ReportedEvent::LogChunk { task_index, data });
        Ok(())
    }
}
