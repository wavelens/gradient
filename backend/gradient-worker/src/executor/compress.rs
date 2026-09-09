/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dump store paths to NAR and compress with zstd before upload.
//!
//! Uses `harmonia-file-nar`'s `NarByteStream` for pure-Rust NAR packing (no `nix nar`
//! subprocess). When the cache is S3-backed the worker uploads the compressed
//! NAR straight to object storage via a presigned PUT URL; otherwise it falls
//! back to chunked [`ClientMessage::NarPush`] over the WebSocket.
//!
//! The worker always compresses before upload; the server never sees or
//! writes an uncompressed NAR.

use anyhow::Result;
use tokio::sync::watch;
use tracing::debug;

use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;

/// Compress every path in `store_paths` into a zstd NAR and upload it to the
/// cache. The worker first asks the server (`CacheQuery {Push}`) how each path
/// should be uploaded: a presigned S3 PUT straight to object storage when the
/// cache is S3-backed, else a direct WebSocket `NarPush`. Used for built
/// outputs so multi-GB NARs never relay through the server connection.
///
/// `abort` is checked before each path. When the server signals `AbortJob`
/// (e.g. the session NAR upload buffer was exceeded) the loop bails with an
/// error so the outer job resolves to `JobFailed` instead of `JobCompleted`.
pub async fn compress_and_push_paths(
    store: &LocalNixStore,
    store_paths: &[String],
    updater: &mut JobUpdater,
    abort: &watch::Receiver<bool>,
) -> Result<()> {
    if store_paths.is_empty() {
        return Ok(());
    }

    updater.report_compressing().await?;

    // Push each output's full runtime closure, not just the output itself: the
    // gradient cache must be closure-complete so a downstream build can fetch
    // every reference (a build's input is a dep output *and its closure*).
    // `upload_one_nar` skips members the cache already holds, so this only
    // uploads paths the cache is missing - e.g. a `-source` referenced by a
    // config that would otherwise strand dependents on `InputsUnavailable`.
    let closure: Vec<String> = store
        .collect_runtime_closure(store_paths)
        .await
        .into_iter()
        .collect();

    let entries = super::query_fetched_paths(updater, closure).await;
    debug!(paths = entries.len(), "compressing and pushing NARs");
    super::upload_all(updater, &entries, Some(store), Some(abort)).await
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use crate::executor::check_abort;
    use tokio::sync::watch;

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use gradient_proto::messages::{CachedPath, ClientMessage, ServerMessage};
    use gradient_test_support::prelude::MockProtoServer;
    use gradient_util::sync::Mutex;

    use crate::connection::{ProtoConnection, ProtoReader};
    use crate::executor::UPLOAD_CONCURRENCY;
    use crate::executor::timeline::JobTimeline;
    use crate::proto::eval_cache_recv::EvalCacheReceiver;
    use crate::proto::job::{DispatchHandle, JobUpdater};
    use crate::proto::nar_recv::NarReceiver;

    fn uncached(path: &str) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: false,
            file_size: None,
            nar_size: None,
            url: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    /// Stand in for the dispatch loop: route the server's resume answers back
    /// to the pushers waiting on their gates.
    fn pump_resumes(mut reader: ProtoReader, nar_recv: NarReceiver) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(inbound) = reader.recv().await {
                if let gradient_proto::Inbound::Control(ServerMessage::NarPushResume {
                    job_id,
                    store_path,
                    received_bytes,
                }) = inbound
                {
                    nar_recv.resolve_push(&job_id, &store_path, received_bytes);
                }
            }
        })
    }

    /// Six paths, one job: the uploader must open exactly `UPLOAD_CONCURRENCY`
    /// streams before any resume answer comes back, which is what hides the
    /// per-path round trip on an eval that pushes hundreds of small NARs.
    #[tokio::test]
    async fn uploads_open_a_window_of_streams_before_any_resume() {
        const PATHS: usize = 6;
        const JOB: &str = "job-upload-window";

        let dir = tempfile::TempDir::new().unwrap();
        let store_paths: Vec<String> = (0..PATHS)
            .map(|i| {
                let path = dir.path().join(format!("{}-p{i}", "a".repeat(32)));
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("hello"), format!("payload {i}")).unwrap();
                path.to_str().unwrap().to_owned()
            })
            .collect();

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;

            // Answer nothing while the window fills: whatever arrives before
            // the first resume IS the window.
            let mut window: Vec<String> = Vec::new();
            while let Ok(msg) = tokio::time::timeout(Duration::from_millis(500), sc.recv()).await {
                match msg.unwrap() {
                    ClientMessage::NarStreamHeader { store_path, .. } => window.push(store_path),
                    other => panic!("expected NarStreamHeader, got {other:?}"),
                }
            }

            let opened = window.len();
            let mut uploaded = 0usize;
            for store_path in window {
                sc.send(ServerMessage::NarPushResume {
                    job_id: JOB.to_owned(),
                    store_path,
                    received_bytes: 0,
                })
                .await
                .unwrap();
            }

            while uploaded < PATHS {
                match sc.recv().await.unwrap() {
                    ClientMessage::NarStreamHeader { store_path, .. } => sc
                        .send(ServerMessage::NarPushResume {
                            job_id: JOB.to_owned(),
                            store_path,
                            received_bytes: 0,
                        })
                        .await
                        .unwrap(),
                    ClientMessage::NarPush { .. } => {}
                    ClientMessage::NarUploaded { .. } => uploaded += 1,
                    other => panic!("unexpected {other:?}"),
                }
            }

            opened
        });

        let conn = ProtoConnection::open(&url).await.unwrap();
        let (writer, reader, _flush) = conn.split();
        let nar_recv = NarReceiver::new();
        let updater = JobUpdater::new(
            JOB.to_owned(),
            DispatchHandle::new("dispatch-1".to_owned()),
            writer,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            nar_recv.clone(),
            EvalCacheReceiver::new(),
            None,
            JobTimeline::new(),
        );
        let pump = pump_resumes(reader, nar_recv);

        let entries: Vec<CachedPath> = store_paths.iter().map(|p| uncached(p)).collect();
        crate::executor::upload_all(&updater, &entries, None, None)
            .await
            .unwrap();

        assert_eq!(server_task.await.unwrap(), UPLOAD_CONCURRENCY);
        pump.abort();
    }

    #[test]
    fn check_abort_returns_ok_when_not_aborted() {
        let (_tx, rx) = watch::channel(false);
        assert!(check_abort(&rx).is_ok());
    }

    #[test]
    fn check_abort_returns_err_after_signal() {
        // Regression: prior to this fix `execute_build_job` ignored the
        // abort watch entirely. With it wired in, `compress_and_push_paths`
        // must surface the abort as an `Err` so the surrounding job
        // resolves to `JobFailed` instead of `JobCompleted`.
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let err = check_abort(&rx).unwrap_err();
        assert!(err.to_string().contains("aborted"), "got: {err}");
    }
}
