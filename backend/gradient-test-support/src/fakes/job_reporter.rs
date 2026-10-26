/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    BuildMetrics, BuildOutput, CachedPath, DiscoveredDerivation, EvalMessageLevel, QueryMode,
};
use gradient_wire::traits::{EvalProgressSink, InputProgressSink, JobReporter};
use gradient_wire::types::{EvalProgress, ImportOutcome, InputOutcome, Realisation};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum ReportedEvent {
    Fetching,
    FetchResult {
        flake_source: Option<String>,
    },
    EvaluatingFlake,
    EvaluatingDerivations,
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    },
    PathsPushed {
        paths: Vec<(String, Option<u64>)>,
    },
    PathsPulled {
        paths: Vec<String>,
    },
    RealisationsRegistered {
        realisations: Vec<Realisation>,
    },
    ImportRequested {
        drv_paths: Vec<String>,
    },
    InputRequested {
        request: String,
    },
    Building {
        build_id: String,
    },
    BuildOutput {
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    },
    LogChunk {
        data: Vec<u8>,
    },
    EvalMessage {
        level: EvalMessageLevel,
        source: String,
        message: String,
    },
    EvalProgress(EvalProgress),
}

#[derive(Debug, Default)]
pub struct RecordingJobReporter {
    events: Arc<Mutex<Vec<ReportedEvent>>>,
    pub cached_paths: Vec<String>,
    pub known_drv_paths: Vec<String>,
    pub known_query_delay: Option<Duration>,
    known_queries_in_flight: Arc<AtomicUsize>,
    peak_known_queries_in_flight: Arc<AtomicUsize>,
    pub upstream: std::collections::HashMap<String, String>,
    pub import_outcomes: std::collections::HashMap<String, ImportOutcome>,
    pub input_outcomes: Vec<(String, InputOutcome)>,
    pub input_progress: Vec<(u64, Option<u64>)>,
}

impl RecordingJobReporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cached_paths(mut self, paths: Vec<String>) -> Self {
        self.cached_paths = paths;
        self
    }

    pub fn with_known_drv_paths(mut self, paths: Vec<String>) -> Self {
        self.known_drv_paths = paths;
        self
    }

    pub fn with_known_query_delay(mut self, delay: Duration) -> Self {
        self.known_query_delay = Some(delay);
        self
    }

    pub fn peak_known_queries_in_flight(&self) -> usize {
        self.peak_known_queries_in_flight.load(Ordering::SeqCst)
    }

    pub fn with_upstream(mut self, path: &str, url: &str) -> Self {
        self.upstream.insert(path.to_owned(), url.to_owned());
        self
    }

    pub fn with_import_outcome(mut self, drv_path: &str, outcome: ImportOutcome) -> Self {
        self.import_outcomes.insert(drv_path.to_owned(), outcome);
        self
    }

    pub fn with_input_outcome(mut self, request_part: &str, outcome: InputOutcome) -> Self {
        self.input_outcomes.push((request_part.to_owned(), outcome));
        self
    }

    pub fn with_input_progress(mut self, bytes_done: u64, bytes_total: Option<u64>) -> Self {
        self.input_progress.push((bytes_done, bytes_total));
        self
    }

    pub fn input_requests(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                ReportedEvent::InputRequested { request } => Some(request),
                _ => None,
            })
            .collect()
    }

    pub fn events(&self) -> Vec<ReportedEvent> {
        self.events.lock().clone()
    }

    fn record(&self, event: ReportedEvent) {
        self.events.lock().push(event);
    }

    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().is_empty()
    }

    pub fn last_eval_result(&self) -> Option<ReportedEvent> {
        self.events()
            .into_iter()
            .rev()
            .find(|e| matches!(e, ReportedEvent::EvalResult { .. }))
    }

    pub fn all_pushed_paths(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                ReportedEvent::PathsPushed { paths } => Some(paths),
                _ => None,
            })
            .flatten()
            .map(|(path, _)| path)
            .collect()
    }

    pub fn all_eval_derivations(&self) -> Vec<DiscoveredDerivation> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                ReportedEvent::EvalResult { derivations, .. } => Some(derivations),
                _ => None,
            })
            .flatten()
            .collect()
    }
}

struct RecordingProgress(Arc<Mutex<Vec<ReportedEvent>>>);

#[async_trait]
impl EvalProgressSink for RecordingProgress {
    async fn report(&self, progress: EvalProgress) {
        self.0.lock().push(ReportedEvent::EvalProgress(progress));
    }
}

#[async_trait]
impl JobReporter for RecordingJobReporter {
    fn eval_progress_sink(&self) -> Arc<dyn EvalProgressSink> {
        Arc::new(RecordingProgress(Arc::clone(&self.events)))
    }

    async fn query_upstream(&self, path: String) -> Result<Option<CachedPath>> {
        Ok(self.upstream.get(&path).map(|url| CachedPath {
            path: path.clone(),
            cached: true,
            file_size: None,
            nar_size: None,
            url: Some(url.clone()),
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }))
    }

    async fn query_known_derivations(&self, drv_paths: Vec<String>) -> Result<Vec<String>> {
        let in_flight = self.known_queries_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_known_queries_in_flight
            .fetch_max(in_flight, Ordering::SeqCst);
        if let Some(delay) = self.known_query_delay {
            tokio::time::sleep(delay).await;
        }
        self.known_queries_in_flight.fetch_sub(1, Ordering::SeqCst);

        let known_set: std::collections::HashSet<&str> =
            self.known_drv_paths.iter().map(|s| s.as_str()).collect();
        Ok(drv_paths
            .into_iter()
            .filter(|p| known_set.contains(p.as_str()))
            .collect())
    }

    async fn query_cache(&self, paths: Vec<String>, mode: QueryMode) -> Result<Vec<CachedPath>> {
        let cached_set: std::collections::HashSet<&str> =
            self.cached_paths.iter().map(|s| s.as_str()).collect();
        Ok(paths
            .into_iter()
            .filter_map(|path| {
                let is_cached = cached_set.contains(path.as_str());
                // Push mode is returning every path with its cached flag. Other modes are returning
                // only cached paths.
                if is_cached || matches!(mode, QueryMode::Push) {
                    Some(CachedPath {
                        path,
                        cached: is_cached,
                        file_size: None,
                        nar_size: None,
                        url: None,
                        nar_hash: None,
                        file_hash: None,
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
        self.record(ReportedEvent::Fetching);
        Ok(())
    }

    async fn report_fetch_result(&mut self, flake_source: Option<String>) -> Result<()> {
        self.record(ReportedEvent::FetchResult { flake_source });
        Ok(())
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.record(ReportedEvent::EvaluatingFlake);
        Ok(())
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.record(ReportedEvent::EvaluatingDerivations);
        Ok(())
    }

    async fn report_eval_result(
        &self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        self.record(ReportedEvent::EvalResult {
            derivations,
            warnings,
            errors,
        });
        Ok(())
    }

    async fn push_paths(&self, paths: &[(String, Option<u64>)]) -> Result<()> {
        self.record(ReportedEvent::PathsPushed {
            paths: paths.to_vec(),
        });
        Ok(())
    }

    async fn pull_paths(&self, paths: Vec<String>) -> Result<()> {
        self.record(ReportedEvent::PathsPulled { paths });
        Ok(())
    }

    async fn register_realisations(&self, realisations: Vec<Realisation>) -> Result<()> {
        self.record(ReportedEvent::RealisationsRegistered { realisations });
        Ok(())
    }

    async fn request_import(&self, drv_paths: Vec<String>) -> Result<ImportOutcome> {
        let outcome = drv_paths
            .iter()
            .find_map(|p| self.import_outcomes.get(p).cloned())
            .unwrap_or(ImportOutcome::Completed {
                realisations: Vec::new(),
            });
        self.record(ReportedEvent::ImportRequested { drv_paths });
        Ok(outcome)
    }

    async fn request_input(
        &self,
        request: String,
        progress: Arc<dyn InputProgressSink>,
    ) -> Result<InputOutcome> {
        let outcome = self
            .input_outcomes
            .iter()
            .find(|(part, _)| request.contains(part.as_str()))
            .map_or_else(
                || InputOutcome::Failed {
                    message: "no input fetch configured".to_owned(),
                },
                |(_, outcome)| outcome.clone(),
            );
        for &(bytes_done, bytes_total) in &self.input_progress {
            progress.transfer(bytes_done, bytes_total);
        }
        self.record(ReportedEvent::InputRequested { request });
        Ok(outcome)
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.record(ReportedEvent::Building { build_id });
        Ok(())
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        self.record(ReportedEvent::BuildOutput {
            build_id,
            outputs,
            metrics,
            substituted,
        });
        Ok(())
    }

    async fn send_log_chunk(&mut self, data: Vec<u8>) -> Result<()> {
        self.record(ReportedEvent::LogChunk { data });
        Ok(())
    }

    async fn send_eval_message(
        &mut self,
        level: EvalMessageLevel,
        source: &str,
        message: &str,
    ) -> Result<()> {
        self.record(ReportedEvent::EvalMessage {
            level,
            source: source.to_owned(),
            message: message.to_owned(),
        });
        Ok(())
    }
}
