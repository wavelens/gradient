/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job scoring — determines how suitable this worker is for each job candidate.
//!
//! The score is computed from the fraction of `required_paths` already present
//! in the local Nix store.  A higher score means fewer paths need downloading,
//! so the worker is a better fit for the job.

use anyhow::Result;
use proto::messages::{CandidateScore, JobCandidate};

/// Computes scores for job candidates against the local Nix store.
pub struct JobScorer;

impl JobScorer {
    pub fn new() -> Self {
        Self
    }

    /// Score a batch of job candidates.
    ///
    /// Each [`CandidateScore`] reports how many of the `required_paths` are
    /// already present locally (higher = better fit).
    ///
    /// TODO(1.1): query the local nix store for path presence.
    pub async fn score_candidates(
        &self,
        candidates: &[JobCandidate],
    ) -> Result<Vec<CandidateScore>> {
        // Skeleton: report score 0 for all candidates until store access is wired up.
        let scores = candidates
            .iter()
            .map(|c| CandidateScore {
                job_id: c.job_id.clone(),
                missing: c.required_paths.len() as u32,
            })
            .collect();
        Ok(scores)
    }
}

impl Default for JobScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn score_empty_candidates() {
        let scorer = JobScorer::new();
        let scores = scorer.score_candidates(&[]).await.unwrap();
        assert!(scores.is_empty());
    }

    #[tokio::test]
    async fn score_sets_missing_to_required_count() {
        let scorer = JobScorer::new();
        let candidates = vec![JobCandidate {
            job_id: "job-1".to_owned(),
            required_paths: vec![
                "/nix/store/aaa".to_owned(),
                "/nix/store/bbb".to_owned(),
                "/nix/store/ccc".to_owned(),
                "/nix/store/ddd".to_owned(),
                "/nix/store/eee".to_owned(),
            ],
        }];
        let scores = scorer.score_candidates(&candidates).await.unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].job_id, "job-1");
        assert_eq!(scores[0].missing, 5);
    }

    #[tokio::test]
    async fn score_multiple_candidates() {
        let scorer = JobScorer::new();
        let candidates = vec![
            JobCandidate { job_id: "a".to_owned(), required_paths: vec![] },
            JobCandidate {
                job_id: "b".to_owned(),
                required_paths: vec!["/nix/store/x".to_owned(), "/nix/store/y".to_owned()],
            },
            JobCandidate {
                job_id: "c".to_owned(),
                required_paths: vec!["/nix/store/z".to_owned()],
            },
        ];
        let scores = scorer.score_candidates(&candidates).await.unwrap();
        assert_eq!(scores.len(), 3);
        let map: std::collections::HashMap<_, _> =
            scores.into_iter().map(|s| (s.job_id, s.missing)).collect();
        assert_eq!(map["a"], 0);
        assert_eq!(map["b"], 2);
        assert_eq!(map["c"], 1);
    }
}
