/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every job kind is ending in this push of only its own outputs.
//! Nothing below an output is looked at.
//! The gate that dispatched the job made its build closure complete in our cache first.
//! The server is requesting a substitute's runtime references itself.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use gradient_util::store_path::nix_store_path;
use gradient_wire::messages::{BuildProgressPhase, BuildStage, CachedPath};
use tokio::sync::watch;

use super::NarUpload;
use crate::proto::job::JobUpdater;
use crate::proto::progress::{BuildProgressSink, Progress, Tally, report_during};
use gradient_worker_client::nar::{NarSource, PathMeta, PathMetaSource, UploadedNar};

pub(crate) struct OutputNar<'a> {
    pub build_id: String,
    pub store_path: String,
    pub source: NarSource<'a>,
}

pub async fn push_outputs(
    updater: &mut JobUpdater,
    outputs: Vec<OutputNar<'_>>,
    abort: &watch::Receiver<bool>,
) -> Result<UploadedNar> {
    if outputs.is_empty() {
        return Ok(UploadedNar::default());
    }

    updater.report_stage(BuildStage::Upload).await?;
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
    let entries = super::query_fetched_paths(updater, paths, sizes).await?;
    let mut sources: HashMap<String, OutputNar<'_>> = outputs
        .into_iter()
        .map(|o| (nix_store_path(&o.store_path), o))
        .collect();
    let pending: Vec<(CachedPath, OutputNar<'_>)> = entries
        .into_iter()
        .filter(|cp| !cp.cached)
        .filter_map(|cp| {
            sources
                .remove(&nix_store_path(&cp.path))
                .map(|output| (cp, output))
        })
        .collect();

    let resolved = ResolvedMeta::of(&pending).await;
    let (mut progress, tallies) = upload_progress(updater, &pending, &resolved);
    let uploads = pending
        .into_iter()
        .map(|(cached, output)| NarUpload {
            tally: tallies[&output.build_id].clone(),
            cached,
            source: resolved.source(output.source),
        })
        .collect();
    let uploaded = report_during(
        &mut progress,
        super::upload_all(updater, uploads, Some(abort)),
    )
    .await;
    for build in &mut progress {
        build.finish().await;
    }
    uploaded
}

fn upload_progress(
    updater: &JobUpdater,
    pending: &[(CachedPath, OutputNar<'_>)],
    resolved: &ResolvedMeta,
) -> (Vec<Progress<BuildProgressSink>>, HashMap<String, Tally>) {
    let mut totals: BTreeMap<&str, (Option<u64>, u32)> = BTreeMap::new();
    for (cached, output) in pending {
        let size = match &output.source {
            NarSource::Raw { nar, .. } => Some(nar.len() as u64),
            NarSource::Path { .. } => resolved.nar_size(&cached.path),
        };
        let (bytes, paths) = totals.entry(&output.build_id).or_insert((Some(0), 0));
        *bytes = bytes.zip(size).map(|(had, more)| had + more);
        *paths += 1;
    }

    let mut tallies = HashMap::new();
    let progress = totals
        .into_iter()
        .map(|(build_id, (bytes, paths))| {
            let mut build = updater.build_progress(build_id.to_owned(), BuildProgressPhase::Upload);
            build.set_total(bytes, paths);
            tallies.insert(build_id.to_owned(), build.tally());
            build
        })
        .collect();
    (progress, tallies)
}

struct ResolvedMeta(HashMap<String, PathMeta>);

impl ResolvedMeta {
    async fn of(pending: &[(CachedPath, OutputNar<'_>)]) -> Self {
        let lookups = pending
            .iter()
            .filter_map(|(cached, output)| match output.source {
                NarSource::Path { meta: Some(meta) } => Some(async move {
                    let path = nix_store_path(&cached.path);
                    let found = meta.path_meta(&path).await;
                    found.map(|found| (path, found))
                }),
                _ => None,
            });
        Self(
            futures::future::join_all(lookups)
                .await
                .into_iter()
                .flatten()
                .collect(),
        )
    }

    fn nar_size(&self, store_path: &str) -> Option<u64> {
        self.0.get(&nix_store_path(store_path))?.nar_size
    }

    fn source<'a>(&'a self, source: NarSource<'a>) -> NarSource<'a> {
        match source {
            NarSource::Path { meta: Some(_) } => NarSource::Path { meta: Some(self) },
            other => other,
        }
    }
}

#[async_trait::async_trait]
impl PathMetaSource for ResolvedMeta {
    async fn path_meta(&self, store_path: &str) -> Option<PathMeta> {
        self.0.get(&nix_store_path(store_path)).cloned()
    }
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

    use gradient_test_support::prelude::MockProtoServer;
    use gradient_util::sync::Mutex;
    use gradient_wire::messages::{CachedPath, ClientMessage, ServerMessage};
    use gradient_wire::types::{GrantTarget, UploadObject, UploadOutcome};

    use crate::executor::timeline::JobTimeline;
    use crate::proto::eval_cache_recv::EvalCacheReceiver;
    use crate::proto::job::JobUpdater;
    use gradient_worker_client::connection::{ProtoConnection, ProtoReader};
    use gradient_worker_client::correlation::{
        AssignmentHandle, CacheWaiters, deliver_cache_reply,
    };
    use gradient_worker_client::nar::NarSource;
    use gradient_worker_client::nar_recv::NarReceiver;
    use gradient_worker_client::upload::UploadClient;

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

    /// The pump must route the `CacheStatus` answer as well as the upload handshake.
    /// [`push_outputs`] is asking the cache first and is timing out without that answer.
    fn pump(
        mut reader: ProtoReader,
        cache_waiters: CacheWaiters,
        uploads: UploadClient,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(msg) = reader.recv().await {
                match msg {
                    ServerMessage::CacheStatus { query_id, cached } => {
                        deliver_cache_reply(&cache_waiters, &query_id, Ok(cached));
                    }
                    msg => uploads.deliver(msg),
                }
            }
        })
    }

    fn updater(
        job: &str,
        writer: gradient_worker_client::connection::ProtoWriter,
        cache_waiters: CacheWaiters,
        uploads: UploadClient,
    ) -> JobUpdater {
        JobUpdater::new(
            job.to_owned(),
            AssignmentHandle::new("dispatch-1".to_owned()),
            writer,
            cache_waiters,
            Arc::new(Mutex::new(HashMap::new())),
            NarReceiver::new(),
            EvalCacheReceiver::new(),
            None,
            JobTimeline::new(),
            uploads,
        )
    }

    #[tokio::test]
    async fn the_uploads_settle_before_upload_all_returns() {
        const PATHS: usize = 3;

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let mut finished = Vec::new();
            while finished.len() < PATHS {
                match sc.recv().await.unwrap() {
                    ClientMessage::UploadRequest { request_id, .. } => sc
                        .send(ServerMessage::UploadGrant {
                            request_id,
                            target: GrantTarget::Passthrough { resume_offset: 0 },
                        })
                        .await
                        .unwrap(),
                    ClientMessage::UploadChunk { .. } => {}
                    ClientMessage::UploadFinished { request_id, .. } => finished.push(request_id),
                    other => panic!("unexpected {other:?}"),
                }
            }
            let withheld = finished.pop().unwrap();
            for request_id in finished {
                sc.send(ServerMessage::UploadCommitted {
                    request_id,
                    outcome: UploadOutcome::Ok,
                })
                .await
                .unwrap();
            }
            release_rx.await.unwrap();
            sc.send(ServerMessage::UploadCommitted {
                request_id: withheld,
                outcome: UploadOutcome::Ok,
            })
            .await
            .unwrap();
        });

        let conn = ProtoConnection::open(&url).await.unwrap();
        let (writer, reader, _flush) = conn.split();
        let uploads = UploadClient::new(writer.clone(), 8);
        let cache_waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let updater = updater(
            "job-upload-settle",
            writer,
            cache_waiters.clone(),
            uploads.clone(),
        );
        let pump = pump(reader, cache_waiters, uploads);

        let task = tokio::spawn(async move {
            let sources = (0..PATHS)
                .map(|i| crate::executor::NarUpload {
                    cached: uncached(&format!("/nix/store/{}-p{i}", "a".repeat(32))),
                    source: NarSource::Raw {
                        nar: b"nar bytes".to_vec(),
                        references: Vec::new(),
                        deriver: None,
                        ca: None,
                    },
                    tally: Default::default(),
                })
                .collect();
            crate::executor::upload_all(&updater, sources, None).await
        });

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !task.is_finished(),
            "an unacknowledged upload keeps the job open"
        );
        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
        server_task.await.unwrap();
        pump.abort();
    }

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
                    ClientMessage::UploadRequest {
                        request_id,
                        object: UploadObject::Nar { store_path },
                        ..
                    } => {
                        opened.push(store_path);
                        sc.send(ServerMessage::UploadGrant {
                            request_id,
                            target: GrantTarget::Passthrough { resume_offset: 0 },
                        })
                        .await
                        .unwrap();
                    }
                    ClientMessage::UploadFinished { request_id, .. } => {
                        sc.send(ServerMessage::UploadCommitted {
                            request_id,
                            outcome: UploadOutcome::Ok,
                        })
                        .await
                        .unwrap();
                        break;
                    }
                    _ => {}
                }
            }
            (queried, opened)
        });

        let conn = ProtoConnection::open(&url).await.unwrap();
        let (writer, reader, _flush) = conn.split();
        let uploads = UploadClient::new(writer.clone(), 8);
        let cache_waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut updater = updater(JOB, writer, cache_waiters.clone(), uploads.clone());
        let pump = pump(reader, cache_waiters, uploads);
        let (_tx, abort) = tokio::sync::watch::channel(false);

        push_outputs(
            &mut updater,
            vec![
                OutputNar {
                    build_id: "b1".to_owned(),
                    store_path: cached.clone(),
                    source: NarSource::Path { meta: None },
                },
                OutputNar {
                    build_id: "b1".to_owned(),
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

    #[tokio::test]
    async fn each_build_reports_the_upload_of_its_own_uncached_outputs() {
        use gradient_wire::messages::BuildProgressPhase;

        let path = |name: &str| format!("/nix/store/{}-{name}", "p".repeat(32));
        let raw = |len: usize| NarSource::Raw {
            nar: vec![0u8; len],
            references: vec![],
            deriver: None,
            ca: None,
        };
        let cached = path("cached");

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let cached_for_server = cached.clone();
        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let mut reports = Vec::new();
            while reports.len() < 2 {
                match sc.recv().await.unwrap() {
                    ClientMessage::CacheQuery {
                        query_id, paths, ..
                    } => {
                        let cached = paths
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
                    ClientMessage::UploadRequest { request_id, .. } => sc
                        .send(ServerMessage::UploadGrant {
                            request_id,
                            target: GrantTarget::Passthrough { resume_offset: 0 },
                        })
                        .await
                        .unwrap(),
                    ClientMessage::UploadFinished { request_id, .. } => sc
                        .send(ServerMessage::UploadCommitted {
                            request_id,
                            outcome: UploadOutcome::Ok,
                        })
                        .await
                        .unwrap(),
                    ClientMessage::BuildProgress {
                        build_id,
                        phase,
                        bytes_done,
                        bytes_total,
                        paths_done,
                        paths_total,
                        ..
                    } => reports.push((
                        build_id,
                        phase,
                        bytes_done,
                        bytes_total,
                        paths_done,
                        paths_total,
                    )),
                    _ => {}
                }
            }
            reports.sort_by(|a, b| a.0.cmp(&b.0));
            reports
        });

        let conn = ProtoConnection::open(&url).await.unwrap();
        let (writer, reader, _flush) = conn.split();
        let uploads = UploadClient::new(writer.clone(), 8);
        let cache_waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut updater = updater(
            "job-upload-progress",
            writer,
            cache_waiters.clone(),
            uploads.clone(),
        );
        let pump = pump(reader, cache_waiters, uploads);
        let (_tx, abort) = tokio::sync::watch::channel(false);
        let output = |build_id: &str, store_path: String, source| OutputNar {
            build_id: build_id.to_owned(),
            store_path,
            source,
        };

        push_outputs(
            &mut updater,
            vec![
                output("b1", path("one"), raw(100)),
                output("b1", path("two"), raw(50)),
                output("b2", cached, raw(999)),
                output("b2", path("three"), raw(30)),
            ],
            &abort,
        )
        .await
        .unwrap();

        let upload = BuildProgressPhase::Upload;
        assert_eq!(
            server_task.await.unwrap(),
            vec![
                ("b1".to_owned(), upload, 150, Some(150), 2, Some(2)),
                ("b2".to_owned(), upload, 30, Some(30), 1, Some(1)),
            ]
        );
        pump.abort();
    }

    #[tokio::test]
    async fn an_output_path_is_looked_up_once_for_its_total_and_its_upload() {
        use super::ResolvedMeta;
        use gradient_worker_client::nar::{PathMeta, PathMetaSource};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counted(AtomicUsize);

        #[async_trait::async_trait]
        impl PathMetaSource for Counted {
            async fn path_meta(&self, _: &str) -> Option<PathMeta> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Some(PathMeta {
                    nar_size: Some(7),
                    ..Default::default()
                })
            }
        }

        let store = Counted(AtomicUsize::new(0));
        let path = format!("/nix/store/{}-out", "o".repeat(32));
        let pending = vec![(
            uncached(&path),
            OutputNar {
                build_id: "b1".to_owned(),
                store_path: path.clone(),
                source: NarSource::Path { meta: Some(&store) },
            },
        )];

        let resolved = ResolvedMeta::of(&pending).await;
        let NarSource::Path {
            meta: Some(upload_meta),
        } = resolved.source(NarSource::Path { meta: Some(&store) })
        else {
            panic!("a path output stays a path upload");
        };

        assert_eq!(resolved.nar_size(&path), Some(7));
        let upload_size = upload_meta.path_meta(&path).await.and_then(|m| m.nar_size);
        assert_eq!(upload_size, Some(7));
        assert_eq!(store.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn check_abort_returns_ok_when_not_aborted() {
        let (_tx, rx) = watch::channel(false);
        assert!(check_abort(&rx).is_ok());
    }

    #[test]
    fn check_abort_returns_err_after_signal() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let err = check_abort(&rx).unwrap_err();
        assert!(err.to_string().contains("aborted"), "got: {err}");
    }
}
