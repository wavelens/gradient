/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background scoring tasks: compute `missing_count` per candidate and send
//! `RequestJobChunk` messages back to the server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use proto::messages::{CandidateScore, JobCandidate};
use tracing::warn;

use crate::connection::ProtoWriter;
use crate::nix::store::LocalNixStore;
use crate::proto::scorer::JobScorer;

// ── Spawn helpers ─────────────────────────────────────────────────────────────

/// Spawn a background scoring task.
///
/// Scores `candidates` using `scorer`, applies delta filtering when
/// `delta_filter` is `true`, and sends `RequestJobChunk` messages to
/// the server.  The final chunk always carries `is_final = true`; the
/// server uses that to know the full submission is complete.
pub(super) fn spawn_scoring_task(
    scorer: JobScorer,
    store: Arc<LocalNixStore>,
    last_scores: Arc<Mutex<HashMap<String, CandidateScore>>>,
    writer: ProtoWriter,
    candidates: Vec<JobCandidate>,
    delta_filter: bool,
    is_final: bool,
) {
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let count = candidates.len();
        let scores = match scorer.score_candidates(&store, &candidates).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, count, "score_candidates failed in spawned task");
                return;
            }
        };

        let to_send: Vec<CandidateScore> = {
            let mut g = last_scores.lock().unwrap();
            let mut out = Vec::with_capacity(scores.len());
            for s in scores {
                if !delta_filter || g.get(&s.job_id) != Some(&s) {
                    g.insert(s.job_id.clone(), s.clone());
                    out.push(s);
                }
            }
            out
        };

        tracing::debug!(
            scored = count,
            sending = to_send.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            is_final,
            "scoring task complete"
        );

        if is_final {
            if let Err(e) = send_score_chunks(&writer, to_send) {
                warn!(error = %e, "send_score_chunks (final) failed");
            }
        } else {
            use proto::messages::ClientMessage;
            for chunk in to_send.chunks(1_000) {
                if let Err(e) = writer.send(ClientMessage::RequestJobChunk {
                    scores: chunk.to_vec(),
                    is_final: false,
                }) {
                    warn!(error = %e, "send RequestJobChunk (non-final) failed");
                    break;
                }
            }
        }
    });
}

/// Send one or more `RequestJobChunk` messages covering all `scores`.
///
/// Always sends at least one message (even when `scores` is empty) so the
/// server sees the `is_final` sentinel.
pub(super) fn send_score_chunks(
    writer: &ProtoWriter,
    scores: Vec<CandidateScore>,
) -> anyhow::Result<()> {
    use proto::messages::ClientMessage;
    if scores.is_empty() {
        writer.send(ClientMessage::RequestJobChunk {
            scores: vec![],
            is_final: true,
        })?;
        return Ok(());
    }
    let chunks: Vec<_> = scores.chunks(1_000).collect();
    let total = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        writer.send(ClientMessage::RequestJobChunk {
            scores: chunk.to_vec(),
            is_final: i + 1 == total,
        })?;
    }
    Ok(())
}
