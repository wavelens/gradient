/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one push every job kind ends in: the outputs it produced, and only those,
//! hashed, compressed and uploaded. Nothing below an output is looked at; the gate
//! that dispatched the job made its build closure whole in our cache first, and a
//! substitute's runtime references are the server's to demand.

use std::collections::HashMap;

use anyhow::Result;
use gradient_proto::messages::CachedPath;
use gradient_sources::nix_store_path;
use tokio::sync::watch;

use crate::proto::job::JobUpdater;
use crate::proto::nar::NarSource;

pub(crate) struct OutputNar<'a> {
    pub store_path: String,
    pub source: NarSource<'a>,
}

pub async fn push_outputs(
    updater: &mut JobUpdater,
    outputs: Vec<OutputNar<'_>>,
    abort: &watch::Receiver<bool>,
) -> Result<()> {
    if outputs.is_empty() {
        return Ok(());
    }

    updater.report_compressing().await?;
    let paths: Vec<String> = outputs
        .iter()
        .map(|o| nix_store_path(&o.store_path))
        .collect();
    let sizes: Vec<Option<u64>> = outputs
        .iter()
        .map(|o| match &o.source {
            NarSource::Raw { nar, .. } => Some(nar.len() as u64),
            NarSource::Path { .. } => None,
        })
        .collect();
    let entries = super::query_fetched_paths(updater, paths, sizes).await;
    let mut sources: HashMap<String, NarSource<'_>> = outputs
        .into_iter()
        .map(|o| (nix_store_path(&o.store_path), o.source))
        .collect();
    let uploads: Vec<(CachedPath, NarSource<'_>)> = entries
        .into_iter()
        .filter(|cp| !cp.cached)
        .filter_map(|cp| {
            sources
                .remove(&nix_store_path(&cp.path))
                .map(|source| (cp, source))
        })
        .collect();

    super::upload_all(updater, uploads, Some(abort)).await
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
    use crate::proto::job::{CacheWaiters, DispatchHandle, JobUpdater, deliver_cache_reply};
    use crate::proto::nar::NarSource;
    use crate::proto::nar_recv::NarReceiver;

    use super::{OutputNar, push_outputs};

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
    /// Stand in for the dispatch loop. It routes the resume a stream waits on AND
    /// the `CacheStatus` a query waits on: [`push_outputs`] asks the cache first,
    /// and a pump that drops that answer leaves the query to time out and every
    /// path to be reported uncached.
    fn pump_resumes(
        mut reader: ProtoReader,
        nar_recv: NarReceiver,
        cache_waiters: CacheWaiters,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(inbound) = reader.recv().await {
                match inbound {
                    gradient_proto::Inbound::Control(ServerMessage::NarPushResume {
                        job_id,
                        store_path,
                        received_bytes,
                    }) => nar_recv.resolve_push(&job_id, &store_path, received_bytes),
                    gradient_proto::Inbound::Control(ServerMessage::CacheStatus {
                        query_id,
                        cached,
                    }) => {
                        deliver_cache_reply(&cache_waiters, &query_id, Ok(cached));
                    }
                    _ => {}
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
        let cache_waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let updater = JobUpdater::new(
            JOB.to_owned(),
            DispatchHandle::new("dispatch-1".to_owned()),
            writer,
            cache_waiters.clone(),
            Arc::new(Mutex::new(HashMap::new())),
            nar_recv.clone(),
            EvalCacheReceiver::new(),
            None,
            JobTimeline::new(),
        );
        let pump = pump_resumes(reader, nar_recv, cache_waiters);

        let entries: Vec<CachedPath> = store_paths.iter().map(|p| uncached(p)).collect();
        crate::executor::upload_all(
            &updater,
            entries
                .into_iter()
                .map(|cp| (cp, NarSource::Path { store: None }))
                .collect(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(server_task.await.unwrap(), UPLOAD_CONCURRENCY);
        pump.abort();
    }

    /// The push is the outputs handed to it and nothing else: one is already in
    /// the cache and is skipped, the other is a raw NAR and is compressed here;
    /// no closure is walked and no third path is ever named to the server.
    #[tokio::test]
    async fn push_outputs_pushes_exactly_what_it_is_given() {
        const JOB: &str = "job-push-outputs";
        let cached = format!("/nix/store/{}-cached", "c".repeat(32));
        let raw_path = format!("/nix/store/{}-raw", "r".repeat(32));
        let raw_nar = b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1\x00\x00\x00".to_vec();

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let cached_for_server = cached.clone();
        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let mut queried: Vec<String> = Vec::new();
            let mut opened: Vec<String> = Vec::new();
            loop {
                match sc.recv().await.unwrap() {
                    ClientMessage::CacheQuery {
                        query_id, paths, ..
                    } => {
                        queried.extend(paths.iter().cloned());
                        let cached: Vec<CachedPath> = paths
                            .into_iter()
                            .map(|p| CachedPath {
                                cached: p == cached_for_server,
                                ..uncached(&p)
                            })
                            .collect();
                        sc.send(ServerMessage::CacheStatus { query_id, cached })
                            .await
                            .unwrap();
                    }
                    ClientMessage::NarStreamHeader { store_path, .. } => {
                        opened.push(store_path.clone());
                        sc.send(ServerMessage::NarPushResume {
                            job_id: JOB.to_owned(),
                            store_path,
                            received_bytes: 0,
                        })
                        .await
                        .unwrap();
                    }
                    ClientMessage::NarUploaded { .. } => break,
                    _ => {}
                }
            }
            (queried, opened)
        });

        let conn = ProtoConnection::open(&url).await.unwrap();
        let (writer, reader, _flush) = conn.split();
        let nar_recv = NarReceiver::new();
        let cache_waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut updater = JobUpdater::new(
            JOB.to_owned(),
            DispatchHandle::new("dispatch-1".to_owned()),
            writer,
            cache_waiters.clone(),
            Arc::new(Mutex::new(HashMap::new())),
            nar_recv.clone(),
            EvalCacheReceiver::new(),
            None,
            JobTimeline::new(),
        );
        let pump = pump_resumes(reader, nar_recv, cache_waiters);
        let (_tx, abort) = tokio::sync::watch::channel(false);

        push_outputs(
            &mut updater,
            vec![
                OutputNar {
                    store_path: cached.clone(),
                    source: NarSource::Path { store: None },
                },
                OutputNar {
                    store_path: raw_path.clone(),
                    source: NarSource::Raw {
                        nar: raw_nar,
                        references: vec![],
                        deriver: None,
                        ca: None,
                    },
                },
            ],
            &abort,
        )
        .await
        .unwrap();

        let (queried, opened) = server_task.await.unwrap();
        assert_eq!(
            queried,
            vec![cached, raw_path.clone()],
            "only the two outputs are named"
        );
        assert_eq!(
            opened,
            vec![raw_path],
            "only the uncached output is streamed"
        );
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
