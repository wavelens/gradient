/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::codec;
use crate::messages::{JobCandidate, PROTO_VERSIONS};
use crate::session::frame::SAFE_INFLIGHT_MESSAGE_SIZE;

const MAX_CANDIDATES_PER_CHUNK: usize = 1_000;

/// A count bound alone let a chunk of build candidates with large closures outgrow the worker's
/// message limit, and the worker dropped its connection on the same offer at each reconnect.
pub fn job_offer_chunks(candidates: &[JobCandidate]) -> Vec<&[JobCandidate]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (i, candidate) in candidates.iter().enumerate() {
        let size = encoded_size(candidate);
        let full =
            i - start == MAX_CANDIDATES_PER_CHUNK || bytes + size > SAFE_INFLIGHT_MESSAGE_SIZE;
        if i > start && full {
            chunks.push(&candidates[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += size;
    }
    if start < candidates.len() {
        chunks.push(&candidates[start..]);
    }
    chunks
}

fn encoded_size(candidate: &JobCandidate) -> usize {
    codec::to_bytes(candidate, *PROTO_VERSIONS.end()).map_or(0, |b| b.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::RequiredPath;

    fn candidate(job: usize, paths: usize) -> JobCandidate {
        JobCandidate {
            job_id: format!("build:{job}"),
            required_paths: (0..paths)
                .map(|p| RequiredPath {
                    path: format!("/nix/store/{p:032}-closure-member-with-a-realistic-name"),
                    cache_info: None,
                })
                .collect(),
            drv_paths: vec![],
            output_paths: vec![],
            requirement: None,
        }
    }

    #[test]
    fn large_candidates_split_below_the_inflight_size_and_keep_their_order() {
        let candidates: Vec<_> = (0..200).map(|j| candidate(j, 300)).collect();

        let chunks = job_offer_chunks(&candidates);

        assert!(
            chunks.len() > 1,
            "a 1000-candidate count bound alone would not split these"
        );
        for chunk in &chunks {
            let bytes: usize = chunk.iter().map(encoded_size).sum();
            assert!(
                bytes <= SAFE_INFLIGHT_MESSAGE_SIZE,
                "chunk of {bytes} bytes"
            );
        }
        let rejoined: Vec<_> = chunks.concat();
        assert_eq!(rejoined, candidates);
    }

    #[test]
    fn small_candidates_still_split_by_count() {
        let candidates: Vec<_> = (0..2_500).map(|j| candidate(j, 0)).collect();

        let sizes: Vec<_> = job_offer_chunks(&candidates)
            .iter()
            .map(|c| c.len())
            .collect();

        assert_eq!(sizes, vec![1_000, 1_000, 500]);
    }
}
