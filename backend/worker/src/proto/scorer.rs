/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job scoring — determines how suitable this worker is for each job candidate.
//!
//! For each candidate the scheduler ships the set of direct input store
//! paths in `required_paths`. The worker checks each path against its local
//! Nix store and reports `(missing_count, missing_nar_size)`. A lower
//! `missing` count means fewer paths need downloading, so the worker is a
//! better fit for the job.
//!
//! Source paths (`inputSrcs`) are not included in `required_paths` — they
//! live only in the `.drv` file and are not stored server-side. They tend
//! to be roughly equivalent across workers in the same org so their absence
//! does not skew scoring meaningfully.

use anyhow::Result;
use proto::messages::{CandidateScore, JobCandidate};
use proto::traits::WorkerStore;
use tracing::debug;

/// Computes scores for job candidates against the local Nix store.
#[derive(Clone, Copy, Default, Debug)]
pub struct JobScorer;

impl JobScorer {
    pub fn new() -> Self {
        Self
    }

    /// Score a batch of job candidates.
    ///
    /// For each candidate, count how many of `required_paths` are absent
    /// from the worker's local store and sum the uncompressed NAR size of
    /// the missing entries (zero when `cache_info` is unavailable).
    pub async fn score_candidates<S: WorkerStore + ?Sized>(
        &self,
        store: &S,
        candidates: &[JobCandidate],
    ) -> Result<Vec<CandidateScore>> {
        let mut scores = Vec::with_capacity(candidates.len());
        for c in candidates {
            let mut missing_count = 0u32;
            let mut missing_nar_size = 0u64;
            for rp in &c.required_paths {
                if !store.has_path(&rp.path).await.unwrap_or(false) {
                    missing_count += 1;
                    missing_nar_size += rp.cache_info.as_ref().map(|ci| ci.nar_size).unwrap_or(0);
                }
            }
            if missing_count > 0 || !c.required_paths.is_empty() {
                debug!(
                    job_id = %c.job_id,
                    required_count = c.required_paths.len(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::messages::{CacheInfo, RequiredPath};
    use test_support::prelude::*;

    #[tokio::test]
    async fn score_empty_candidates() {
        let store = FakeWorkerStore::new();
        let scores = JobScorer::new().score_candidates(&store, &[]).await.unwrap();
        assert!(scores.is_empty());
    }

    #[tokio::test]
    async fn score_eval_job_always_zero() {
        let store = FakeWorkerStore::new();
        let candidates = vec![JobCandidate {
            job_id: "eval:1".to_owned(),
            required_paths: vec![],
            drv_paths: vec![],
        }];
        let scores = JobScorer::new()
            .score_candidates(&store, &candidates)
            .await
            .unwrap();
        assert_eq!(scores[0].missing_count, 0);
        assert_eq!(scores[0].missing_nar_size, 0);
    }

    #[tokio::test]
    async fn score_counts_missing_required_paths() {
        let store = FakeWorkerStore::new().with_present_path("/nix/store/aaaa-have");
        let candidates = vec![JobCandidate {
            job_id: "build:1".to_owned(),
            required_paths: vec![
                RequiredPath {
                    path: "/nix/store/aaaa-have".to_owned(),
                    cache_info: Some(CacheInfo { file_size: 10, nar_size: 100 }),
                },
                RequiredPath {
                    path: "/nix/store/bbbb-missing".to_owned(),
                    cache_info: Some(CacheInfo { file_size: 20, nar_size: 200 }),
                },
                RequiredPath {
                    path: "/nix/store/cccc-missing-no-info".to_owned(),
                    cache_info: None,
                },
            ],
            drv_paths: vec!["/nix/store/zzzz-target.drv".to_owned()],
        }];
        let scores = JobScorer::new()
            .score_candidates(&store, &candidates)
            .await
            .unwrap();
        assert_eq!(scores[0].missing_count, 2);
        assert_eq!(scores[0].missing_nar_size, 200);
    }
}
