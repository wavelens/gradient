/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Source paths (`inputSrcs`) are not part of `required_paths`.
//! They are living only in the `.drv` file and are not stored server-side.
//! They are roughly equal across workers of one project and barely skew scoring.

use anyhow::Result;
use gradient_wire::messages::{CandidateScore, JobCandidate};
use gradient_wire::traits::WorkerStore;
use tracing::debug;

#[derive(Clone, Copy, Default, Debug)]
pub struct JobScorer;

impl JobScorer {
    pub fn new() -> Self {
        Self
    }

    pub async fn score_candidates<S: WorkerStore + ?Sized>(
        &self,
        store: &S,
        candidates: &[JobCandidate],
    ) -> Result<Vec<CandidateScore>> {
        let mut scores = Vec::with_capacity(candidates.len());
        for c in candidates {
            let (missing_count, missing_nar_size) = missing_inputs(store, c).await;
            let outputs_present = holds_every_output(store, c).await;
            if missing_count > 0 || !c.required_paths.is_empty() || outputs_present {
                debug!(
                    job_id = %c.job_id,
                    required_count = c.required_paths.len(),
                    missing_count,
                    missing_nar_size,
                    outputs_present,
                    "scored candidate"
                );
            }
            scores.push(CandidateScore {
                job_id: c.job_id.clone(),
                missing_count,
                missing_nar_size,
                outputs_present,
            });
        }
        Ok(scores)
    }
}

async fn missing_inputs<S: WorkerStore + ?Sized>(store: &S, c: &JobCandidate) -> (u32, u64) {
    let mut missing_count = 0u32;
    let mut missing_nar_size = 0u64;
    for rp in &c.required_paths {
        if !store.has_path(&rp.path).await.unwrap_or(false) {
            missing_count += 1;
            missing_nar_size += rp.cache_info.as_ref().map(|ci| ci.nar_size).unwrap_or(0);
        }
    }
    (missing_count, missing_nar_size)
}

async fn holds_every_output<S: WorkerStore + ?Sized>(store: &S, c: &JobCandidate) -> bool {
    if c.output_paths.is_empty() {
        return false;
    }
    for path in &c.output_paths {
        if !store.has_path(path).await.unwrap_or(false) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::prelude::*;
    use gradient_wire::messages::{CacheInfo, RequiredPath};

    #[tokio::test]
    async fn score_empty_candidates() {
        let store = FakeWorkerStore::new();
        let scores = JobScorer::new()
            .score_candidates(&store, &[])
            .await
            .unwrap();
        assert!(scores.is_empty());
    }

    #[tokio::test]
    async fn score_eval_job_always_zero() {
        let store = FakeWorkerStore::new();
        let candidates = vec![JobCandidate {
            job_id: "eval:1".to_owned(),
            required_paths: vec![],
            drv_paths: vec![],
            output_paths: vec![],
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
                    cache_info: Some(CacheInfo {
                        file_size: 10,
                        nar_size: 100,
                    }),
                },
                RequiredPath {
                    path: "/nix/store/bbbb-missing".to_owned(),
                    cache_info: Some(CacheInfo {
                        file_size: 20,
                        nar_size: 200,
                    }),
                },
                RequiredPath {
                    path: "/nix/store/cccc-missing-no-info".to_owned(),
                    cache_info: None,
                },
            ],
            drv_paths: vec!["/nix/store/zzzz-target.drv".to_owned()],
            output_paths: vec![],
        }];
        let scores = JobScorer::new()
            .score_candidates(&store, &candidates)
            .await
            .unwrap();
        assert_eq!(scores[0].missing_count, 2);
        assert_eq!(scores[0].missing_nar_size, 200);
    }

    fn build_candidate(output_paths: &[&str]) -> JobCandidate {
        JobCandidate {
            job_id: "build:1".to_owned(),
            required_paths: vec![],
            drv_paths: vec!["/nix/store/zzzz-target.drv".to_owned()],
            output_paths: output_paths.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn score_reports_outputs_present_only_when_every_output_is_held() {
        let store = FakeWorkerStore::new()
            .with_present_path("/nix/store/aaaa-out")
            .with_present_path("/nix/store/bbbb-dev");
        let candidates = vec![
            build_candidate(&["/nix/store/aaaa-out", "/nix/store/bbbb-dev"]),
            build_candidate(&["/nix/store/aaaa-out", "/nix/store/cccc-doc"]),
            build_candidate(&[]),
        ];
        let scores = JobScorer::new()
            .score_candidates(&store, &candidates)
            .await
            .unwrap();
        let present: Vec<bool> = scores.iter().map(|s| s.outputs_present).collect();
        assert_eq!(present, [true, false, false]);
    }
}
