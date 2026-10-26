/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use anyhow::{Result, bail};
use futures::StreamExt as _;
use gradient_storage::NarStore;
use gradient_types::ids::CacheId;
use gradient_wire::types::{EvalCachePullOutcome, SegmentFetch, SegmentSource};
use tracing::warn;

use super::socket::{BULK_CHUNK_SIZE, ProtoWriter, send_server_msg};
use gradient_wire::messages::{PRESIGN_TTL, ServerMessage};

pub(super) async fn handle_eval_cache_pull(
    nar_storage: &NarStore,
    writer: &ProtoWriter,
    readable: &[CacheId],
    job_id: String,
    fingerprint: String,
    applied: Vec<String>,
) {
    let (outcome, inline) = segments_outcome(nar_storage, readable, &fingerprint, applied).await;

    if send_server_msg(
        writer,
        &ServerMessage::EvalCachePullResult {
            job_id: job_id.clone(),
            outcome,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    for key in inline {
        if let Err(e) = stream_blob_inline(nar_storage, writer, &job_id, &key).await {
            warn!(%fingerprint, %key, error = %e, "inline eval cache segment stream failed");
        }
    }
}

async fn segments_outcome(
    nar_storage: &NarStore,
    readable: &[CacheId],
    fingerprint: &str,
    applied: Vec<String>,
) -> (EvalCachePullOutcome, Vec<String>) {
    let listed = futures::future::join_all(readable.iter().map(|&cache| async move {
        (
            cache,
            nar_storage
                .list_eval_cache_segments(cache, fingerprint)
                .await,
        )
    }))
    .await;
    let applied: HashSet<String> = applied.into_iter().collect();
    let (keys, send) = select_segments(listed, &applied);
    if keys.is_empty() {
        return (EvalCachePullOutcome::Miss, Vec::new());
    }

    let fetch = futures::future::join_all(
        send.into_iter()
            .map(|(key, size)| segment_fetch(nar_storage, key, size)),
    )
    .await;
    let inline: Vec<String> = fetch
        .iter()
        .filter(|f| matches!(f.source, SegmentSource::Inline { .. }))
        .map(|f| f.key.clone())
        .collect();
    let stream_token = (!inline.is_empty()).then(|| stream_token(fingerprint));
    let outcome = EvalCachePullOutcome::Segments {
        keys,
        fetch,
        stream_token,
    };
    (outcome, inline)
}

type CacheListing = (CacheId, Result<Vec<(String, u64)>>);

fn select_segments(
    listed: Vec<CacheListing>,
    applied: &HashSet<String>,
) -> (Vec<String>, Vec<(String, u64)>) {
    let mut keys = Vec::new();
    let mut send = Vec::new();
    for (cache, segments) in listed {
        let segments = match segments {
            Ok(segments) => segments,
            Err(e) => {
                warn!(%cache, error = %e, "listing eval cache segments failed");
                continue;
            }
        };

        for (key, size) in segments {
            if !applied.contains(&key) {
                send.push((key.clone(), size));
            }

            keys.push(key);
        }
    }

    (keys, send)
}

async fn segment_fetch(nar_storage: &NarStore, key: String, size: u64) -> SegmentFetch {
    let url = nar_storage
        .presigned_eval_cache_get_url(&key, PRESIGN_TTL)
        .await
        .unwrap_or_else(|e| {
            warn!(%key, error = %e, "presigned eval cache GET failed, falling back to inline");
            None
        });
    let source = match url {
        Some(url) => SegmentSource::Presigned { url },
        None => SegmentSource::Inline { size_bytes: size },
    };
    SegmentFetch { key, source }
}

struct ChunkSender<'a> {
    writer: &'a ProtoWriter,
    job_id: &'a str,
    buf: Vec<u8>,
    offset: u64,
}

impl ChunkSender<'_> {
    async fn push(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let take = bytes.len().min(BULK_CHUNK_SIZE - self.buf.len());
            self.buf.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buf.len() == BULK_CHUNK_SIZE {
                self.send(false).await?;
            }
        }

        Ok(())
    }

    async fn send(&mut self, is_final: bool) -> Result<()> {
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(BULK_CHUNK_SIZE));
        let len = chunk.len() as u64;
        let sent = send_server_msg(
            self.writer,
            &ServerMessage::EvalCacheChunk {
                job_id: self.job_id.to_owned(),
                data: chunk.into(),
                offset: self.offset,
                is_final,
            },
        )
        .await;
        if sent.is_err() {
            bail!("eval cache send stalled at offset {}", self.offset);
        }

        self.offset += len;
        Ok(())
    }
}

async fn stream_blob_inline(
    nar_storage: &NarStore,
    writer: &ProtoWriter,
    job_id: &str,
    key: &str,
) -> Result<()> {
    let mut sender = ChunkSender {
        writer,
        job_id,
        buf: Vec::with_capacity(BULK_CHUNK_SIZE),
        offset: 0,
    };
    let read = read_blob(nar_storage, key, &mut sender).await;
    // A segment that fails mid-read still ends with a final chunk, so the next one starts clean.
    sender.send(true).await?;
    read
}

async fn read_blob(nar_storage: &NarStore, key: &str, sender: &mut ChunkSender<'_>) -> Result<()> {
    let Some((_size, mut stream)) = nar_storage.get_eval_cache_stream(key).await? else {
        bail!("eval cache segment {key} vanished before stream");
    };

    while let Some(item) = stream.next().await {
        sender.push(&item?).await?;
    }

    Ok(())
}

fn stream_token(fingerprint: &str) -> String {
    format!("ec-{fingerprint}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::eval_cache_segment::eval_cache_segment_key;
    use uuid::Uuid;

    fn segment(cache: CacheId) -> (String, u64) {
        (eval_cache_segment_key(cache, "fp", Uuid::now_v7()), 10)
    }

    fn decode(bytes: bytes::Bytes) -> ServerMessage {
        gradient_wire::codec::from_bytes::<ServerMessage>(
            bytes,
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap()
    }

    #[test]
    fn select_segments_sends_only_unapplied_keys() {
        let (first, second) = (CacheId::now_v7(), CacheId::now_v7());
        let (a, b, c) = (segment(first), segment(first), segment(second));
        let applied = HashSet::from([b.0.clone()]);

        let (keys, send) = select_segments(
            vec![
                (first, Ok(vec![a.clone(), b.clone()])),
                (second, Ok(vec![c.clone()])),
            ],
            &applied,
        );

        assert_eq!(keys, vec![a.0.clone(), b.0, c.0.clone()]);
        assert_eq!(send, vec![a, c]);
    }

    #[test]
    fn a_failed_listing_drops_only_that_cache() {
        let (broken, healthy) = (CacheId::now_v7(), CacheId::now_v7());
        let kept = segment(healthy);

        let (keys, send) = select_segments(
            vec![
                (broken, Err(anyhow::anyhow!("listing timed out"))),
                (healthy, Ok(vec![kept.clone()])),
            ],
            &HashSet::new(),
        );

        assert_eq!(keys, vec![kept.0.clone()]);
        assert_eq!(send, vec![kept]);
    }

    #[tokio::test]
    async fn a_project_without_readable_caches_misses() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        let (writer, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        handle_eval_cache_pull(&store, &writer, &[], "j1".into(), "fp".into(), Vec::new()).await;

        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::EvalCachePullResult {
                outcome: EvalCachePullOutcome::Miss,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn inline_segments_stream_back_to_back_in_fetch_order() {
        let cache = CacheId::now_v7();
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        let keys: Vec<String> = (0..3)
            .map(|_| eval_cache_segment_key(cache, "fp", Uuid::now_v7()))
            .collect();
        for (key, body) in keys.iter().zip([&b"first"[..], b"second", b"third"]) {
            store.put_eval_cache(key, body.to_vec()).await.unwrap();
        }
        let (writer, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        handle_eval_cache_pull(
            &store,
            &writer,
            &[cache],
            "j1".into(),
            "fp".into(),
            vec![keys[1].clone()],
        )
        .await;

        let ServerMessage::EvalCachePullResult {
            outcome:
                EvalCachePullOutcome::Segments {
                    keys: listed,
                    fetch,
                    stream_token,
                },
            ..
        } = decode(sent.try_recv().unwrap())
        else {
            panic!("expected segments")
        };
        assert_eq!(listed, keys);
        assert_eq!(
            fetch,
            vec![
                SegmentFetch {
                    key: keys[0].clone(),
                    source: SegmentSource::Inline { size_bytes: 5 },
                },
                SegmentFetch {
                    key: keys[2].clone(),
                    source: SegmentSource::Inline { size_bytes: 5 },
                },
            ]
        );
        assert_eq!(stream_token, Some("ec-fp".into()));

        let chunks: Vec<(Vec<u8>, u64, bool)> = std::iter::from_fn(|| sent.try_recv().ok())
            .map(|bytes| match decode(bytes) {
                ServerMessage::EvalCacheChunk {
                    data,
                    offset,
                    is_final,
                    ..
                } => (data.to_vec(), offset, is_final),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            chunks,
            vec![(b"first".to_vec(), 0, true), (b"third".to_vec(), 0, true)]
        );
    }
}
