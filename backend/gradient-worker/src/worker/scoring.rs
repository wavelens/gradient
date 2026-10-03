/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_wire::messages::{CandidateScore, JobCandidate, JobKind};
use gradient_wire::traits::WorkerStore;
use tracing::{Instrument as _, warn};

use crate::proto::scorer::JobScorer;
use gradient_worker_client::connection::ProtoWriter;

pub(super) fn spawn_scoring_task<S: WorkerStore + ?Sized + 'static>(
    scorer: JobScorer,
    store: Arc<S>,
    writer: ProtoWriter,
    candidates: Vec<JobCandidate>,
    is_final: bool,
    request_after: Vec<JobKind>,
) {
    #[expect(
        clippy::disallowed_methods,
        reason = "reports through the writer; the connection going away drops it"
    )]
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let count = candidates.len();
        let to_send = match scorer
            .score_candidates(&*store, &candidates)
            .instrument(tracing::debug_span!("score_candidates", count))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, count, "score_candidates failed in spawned task");
                return;
            }
        };

        tracing::debug!(
            scored = count,
            elapsed_ms = started.elapsed().as_millis() as u64,
            is_final,
            "scoring task complete"
        );

        use gradient_wire::messages::ClientMessage;
        if is_final {
            if let Err(e) = send_score_chunks(&writer, to_send)
                .instrument(tracing::debug_span!("send_scores"))
                .await
            {
                warn!(error = %e, "send_score_chunks (final) failed");
            }
        } else {
            for chunk in to_send.chunks(1_000) {
                if let Err(e) = writer
                    .send(ClientMessage::RequestJobChunk {
                        scores: chunk.to_vec(),
                        is_final: false,
                    })
                    .await
                {
                    warn!(error = %e, "send RequestJobChunk (non-final) failed");
                    break;
                }
            }
        }

        // Scoring a fresh offer is clearing the server's rescore gate. Requesting here is sparing a
        // dependency chain the wait for the next heartbeat.
        for kind in request_after {
            if let Err(e) = writer
                .send(ClientMessage::RequestJob { kind: kind.clone() })
                .instrument(tracing::debug_span!("request_job", ?kind))
                .await
            {
                warn!(error = %e, ?kind, "RequestJob after scoring failed");
            }
        }
    });
}

/// At least one message must go out, even with no scores. The server is waiting for the `is_final`
/// sentinel.
async fn send_score_chunks(
    writer: &ProtoWriter,
    scores: Vec<CandidateScore>,
) -> anyhow::Result<()> {
    use gradient_wire::messages::ClientMessage;
    if scores.is_empty() {
        writer
            .send(ClientMessage::RequestJobChunk {
                scores: vec![],
                is_final: true,
            })
            .await?;
        return Ok(());
    }
    let chunks: Vec<_> = scores.chunks(1_000).collect();
    let total = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        writer
            .send(ClientMessage::RequestJobChunk {
                scores: chunk.to_vec(),
                is_final: i + 1 == total,
            })
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::prelude::{FakeWorkerStore, MockProtoServer};
    use gradient_worker_client::connection::ProtoConnection;

    #[tokio::test]
    async fn a_candidate_offered_again_is_scored_again() {
        let server = MockProtoServer::bind().await;
        let (mut sc, conn) = tokio::join!(server.accept(), ProtoConnection::open(server.url()));
        let (writer, _reader, _flush) = conn.expect("the mock server accepts").split();
        let store = Arc::new(FakeWorkerStore::new());
        let candidate = JobCandidate {
            job_id: "build:1".to_owned(),
            required_paths: vec![],
            drv_paths: vec!["/nix/store/zzzz-target.drv".to_owned()],
            output_paths: vec![],
            requirement: None,
        };

        for offer in ["first", "repeated"] {
            spawn_scoring_task(
                JobScorer::new(),
                Arc::clone(&store),
                writer.clone(),
                vec![candidate.clone()],
                true,
                Vec::new(),
            );
            let scores = sc.scores().await.expect("the worker answers");
            let ids: Vec<&str> = scores.iter().map(|s| s.job_id.as_str()).collect();
            assert_eq!(ids, ["build:1"], "the {offer} offer is scored");
        }
    }
}
