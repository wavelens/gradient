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
