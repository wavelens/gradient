/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job scoring — determines how suitable this worker is for each job candidate.
//!
//! For build candidates the scorer reads the `.drv` file, extracts every input
//! store path (`inputDrvs` outputs + `inputSrcs`), and checks each against the
//! local Nix store.  A lower `missing` count means fewer paths need
//! downloading, so the worker is a better fit for the job.

use anyhow::Result;
use gradient_core::db::parse_drv;
use gradient_core::executer::path_utils::nix_store_path;
use proto::messages::{CandidateScore, JobCandidate, RequiredPath};
use tracing::{debug, warn};

use crate::nix::store::LocalNixStore;

/// Computes scores for job candidates against the local Nix store.
#[derive(Clone, Copy, Default, Debug)]
pub struct JobScorer;

impl JobScorer {
    pub fn new() -> Self {
        Self
    }

    /// Score a batch of job candidates.
    ///
    /// For build jobs (`drv_paths` non-empty), reads each `.drv` file to
    /// discover the full set of input store paths and checks each against
    /// the local store.  For eval jobs, always returns `missing_count: 0`.
    ///
    /// `missing_nar_size` is computed from `CacheInfo.nar_size` for any
    /// required path not present locally; paths without cache info contribute 0.
    pub async fn score_candidates(
        &self,
        store: &LocalNixStore,
        candidates: &[JobCandidate],
    ) -> Result<Vec<CandidateScore>> {
        let mut scores = Vec::with_capacity(candidates.len());
        for c in candidates {
            let (missing_count, missing_nar_size) = if c.drv_paths.is_empty() {
                // Eval job — no store check needed.
                (0, 0)
            } else {
                Self::count_missing_inputs(store, &c.drv_paths, &c.required_paths).await
            };
            if missing_count > 0 || !c.drv_paths.is_empty() {
                debug!(
                    job_id = %c.job_id,
                    drv_count = c.drv_paths.len(),
                    missing_count,
                    missing_nar_size,
                    "scored candidate"
                );
            }
            scores.push(CandidateScore {
                job_id: c.job_id.clone(),
                missing_count,
                missing_nar_size,
            });
        }
        Ok(scores)
    }

    /// Read `.drv` files, collect all input store paths, and count how
    /// many are absent from the local Nix store. Also sums up the NAR size
    /// of missing paths using `CacheInfo` when available.
    async fn count_missing_inputs(
        store: &LocalNixStore,
        drv_paths: &[String],
        required_paths: &[RequiredPath],
    ) -> (u32, u64) {
        let mut missing_count = 0u32;
        let mut missing_nar_size = 0u64;

        // Build a lookup map from store path → nar_size for fast access.
        let path_nar_size: std::collections::HashMap<&str, u64> = required_paths
            .iter()
            .filter_map(|rp| {
                rp.cache_info
                    .as_ref()
                    .map(|ci| (rp.path.as_str(), ci.nar_size))
            })
            .collect();

        for drv_path in drv_paths {
            let full_path = nix_store_path(drv_path);
            let drv_bytes = match tokio::fs::read(&full_path).await {
                Ok(b) => b,
                Err(e) => {
                    // Can't read the .drv → count as 1 missing (the drv itself).
                    warn!(drv = %full_path, error = %e, "cannot read .drv for scoring");
                    missing_count += 1;
                    continue;
                }
            };
            let drv = match parse_drv(&drv_bytes) {
                Ok(d) => d,
                Err(e) => {
                    warn!(drv = %full_path, error = %e, "cannot parse .drv for scoring");
                    missing_count += 1;
                    continue;
                }
            };

            // Check inputDrvs output paths.
            for (input_drv, _outputs) in &drv.input_derivations {
                let input_full = nix_store_path(input_drv);
                // Read the input .drv to get its output paths.
                if let Ok(input_bytes) = tokio::fs::read(&input_full).await
                    && let Ok(input_parsed) = parse_drv(&input_bytes) {
                        for o in &input_parsed.outputs {
                            if o.path.is_empty() {
                                continue;
                            }
                            match store.has_path(&o.path).await {
                                Ok(true) => {}
                                _ => {
                                    missing_count += 1;
                                    missing_nar_size +=
                                        path_nar_size.get(o.path.as_str()).copied().unwrap_or(0);
                                }
                            }
                        }
                    }
            }

            // Check inputSrcs.
            for src_path in &drv.input_sources {
                match store.has_path(src_path).await {
                    Ok(true) => {}
                    _ => {
                        missing_count += 1;
                        missing_nar_size +=
                            path_nar_size.get(src_path.as_str()).copied().unwrap_or(0);
                    }
                }
            }
        }
        (missing_count, missing_nar_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::traits::WorkerStore;
    use test_support::prelude::*;

    #[tokio::test]
    async fn score_empty_candidates() {
        let store = FakeWorkerStore::new();
        let scorer = JobScorer::new();
        let scores = scorer
            .score_candidates_with_store(&store, &[])
            .await
            .unwrap();
        assert!(scores.is_empty());
    }

    #[tokio::test]
    async fn score_eval_job_always_zero() {
        let store = FakeWorkerStore::new();
        let scorer = JobScorer::new();
        let candidates = vec![JobCandidate {
            job_id: "eval:1".to_owned(),
            required_paths: vec![],
            drv_paths: vec![], // eval job — no drv_paths
        }];
        let scores = scorer
            .score_candidates_with_store(&store, &candidates)
            .await
            .unwrap();
        assert_eq!(scores[0].missing_count, 0);
        assert_eq!(scores[0].missing_nar_size, 0);
    }

    #[tokio::test]
    async fn score_build_with_missing_drv_file() {
        let store = FakeWorkerStore::new();
        let scorer = JobScorer::new();
        let candidates = vec![JobCandidate {
            job_id: "build:1".to_owned(),
            required_paths: vec![],
            drv_paths: vec!["/nix/store/nonexistent.drv".to_owned()],
        }];
        let scores = scorer
            .score_candidates_with_store(&store, &candidates)
            .await
            .unwrap();
        // Can't read the drv → at least 1 missing.
        assert!(scores[0].missing_count >= 1);
    }

    impl JobScorer {
        /// Test helper using FakeWorkerStore for required_paths checking.
        async fn score_candidates_with_store(
            &self,
            store: &FakeWorkerStore,
            candidates: &[JobCandidate],
        ) -> Result<Vec<CandidateScore>> {
            let mut scores = Vec::with_capacity(candidates.len());
            for c in candidates {
                let mut missing_count = 0u32;
                let mut missing_nar_size = 0u64;
                // For tests without .drv files, fall back to required_paths check.
                for rp in &c.required_paths {
                    match store.has_path(&rp.path).await {
                        Ok(true) => {}
                        _ => {
                            missing_count += 1;
                            missing_nar_size +=
                                rp.cache_info.as_ref().map(|ci| ci.nar_size).unwrap_or(0);
                        }
                    }
                }
                // Count each unreadable drv as missing.
                for drv_path in &c.drv_paths {
                    if tokio::fs::metadata(nix_store_path(drv_path)).await.is_err() {
                        missing_count += 1;
                    }
                }
                scores.push(CandidateScore {
                    job_id: c.job_id.clone(),
                    missing_count,
                    missing_nar_size,
                });
            }
            Ok(scores)
        }
    }
}
