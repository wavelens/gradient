/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bytes::Bytes;
use gradient_wire::messages::{
    BuildMetrics, BuildOutput, CachedPath, ClientMessage, DiscoveredDerivation,
    EvalCachePullOutcome, EvalMessageLevel, EvalStatsReport, JobPhase, JobUpdateKind, QueryMode,
};
use gradient_worker_client::correlation::{
    AssignmentHandle, CacheWaiters, ImportWaiters, InputWaiters, KnownDerivationWaiters,
    cache_query_with_timeout, known_derivations_with_timeout,
};
use tracing::{debug, warn};

use crate::count::clamp_to_u32;
use crate::executor::timeline::{JobTimeline, PhaseGuard};
use crate::nix::store::LocalNixStore;
use crate::proto::eval_cache_recv::{EvalCacheReceiver, PendingPull};
use crate::proto::eval_shard_replies::EvalShardReplies;
use crate::proto::job_log::JobLog;
use crate::proto::prefetch::MissingInputs;
use crate::proto::progress::{
    BuildProgressSink, EvalProgressSender, Progress, Tally, count_delivery,
};
use gradient_wire::traits::{EvalProgressSink, InputProgressSink, JobReporter};
use gradient_wire::types::{
    BuildProgressPhase, BuildStage, EvalShardOutcome, EvalShardSpec, FinishedShard, GrantTarget,
    ImportOutcome, InputOutcome, Realisation, SegmentFetch, SegmentSource, UploadMetadata,
    UploadObject,
};
use gradient_worker_client::connection::reconnecting::ReconnectingWriter;
use gradient_worker_client::nar_recv::{NarPayload, NarReceiver, NarUnavailable};
use gradient_worker_client::upload::UploadClient;

pub struct JobUpdater {
    pub(crate) job_id: String,
    pub(crate) assignment_id: AssignmentHandle,
    pub(crate) writer: ReconnectingWriter,
    pub(crate) cache_waiters: CacheWaiters,
    pub(crate) known_derivation_waiters: KnownDerivationWaiters,
    pub(crate) import_waiters: ImportWaiters,
    pub(crate) input_waiters: InputWaiters,
    pub(crate) nar_recv: NarReceiver,
    pub(crate) eval_cache_recv: EvalCacheReceiver,
    pub(crate) eval_shard_replies: EvalShardReplies,
    pub(crate) store: Option<Arc<LocalNixStore>>,
    pub(crate) timeline: Arc<JobTimeline>,
    pub(crate) uploads: UploadClient,
    pub(crate) log: JobLog,
}

async fn passthrough_blob(
    uploads: &UploadClient,
    request_id: u64,
    bytes: &[u8],
    resume_offset: u64,
) -> Result<()> {
    let writer = uploads.writer();
    let start = usize::try_from(resume_offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let mut offset = start as u64;
    for chunk in bytes[start..].chunks(uploads.chunk_bytes()) {
        writer
            .send_now(ClientMessage::UploadChunk {
                request_id,
                data: Bytes::copy_from_slice(chunk),
                offset,
                is_final: false,
            })
            .await?;
        offset += chunk.len() as u64;
    }
    writer
        .send_now(ClientMessage::UploadChunk {
            request_id,
            data: Bytes::new(),
            offset,
            is_final: true,
        })
        .await
}

#[derive(Debug, Default)]
pub struct PulledSegments {
    pub keys: Vec<String>,
    pub segments: Vec<(String, Vec<u8>)>,
}

async fn fetch_segments(
    fetch: Vec<SegmentFetch>,
    pending: &mut PendingPull,
) -> Vec<(String, Vec<u8>)> {
    let presigned = futures::future::join_all(fetch.iter().filter_map(|f| match &f.source {
        SegmentSource::Presigned { url } => Some(download_segment(url)),
        SegmentSource::Inline { .. } => None,
    }));
    let inline = async {
        let mut streamed = Vec::new();
        for f in &fetch {
            if let SegmentSource::Inline { size_bytes } = f.source {
                streamed.push(inline_segment(pending, size_bytes).await);
            }
        }

        streamed
    };
    let (downloaded, streamed) = tokio::join!(presigned, inline);

    let mut downloaded = downloaded.into_iter();
    let mut streamed = streamed.into_iter();
    fetch
        .into_iter()
        .filter_map(|f| {
            let fetched = match f.source {
                SegmentSource::Presigned { .. } => downloaded.next(),
                SegmentSource::Inline { .. } => streamed.next(),
            }?;
            match fetched {
                Ok(bytes) => Some((f.key, bytes)),
                Err(e) => {
                    warn!(key = %f.key, error = %e, "eval cache segment fetch failed; skipping it");
                    None
                }
            }
        })
        .collect()
}

async fn inline_segment(pending: &mut PendingPull, size_bytes: u64) -> Result<Vec<u8>> {
    let bytes = pending.await_inline_segment().await?;
    if bytes.len() as u64 != size_bytes {
        bail!(
            "inline eval cache segment holds {} bytes, {size_bytes} announced",
            bytes.len()
        );
    }

    Ok(bytes)
}

async fn download_segment(url: &str) -> Result<Vec<u8>> {
    Ok(gradient_worker_client::http::client()
        .get(url)
        .send()
        .await
        .with_context(|| format!("eval-cache GET {url}"))?
        .error_for_status()
        .with_context(|| format!("eval-cache GET {url} returned non-2xx"))?
        .bytes()
        .await
        .with_context(|| format!("read eval-cache body of {url}"))?
        .to_vec())
}

impl JobUpdater {
    #[expect(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn new(
        job_id: String,
        assignment_id: AssignmentHandle,
        writer: ReconnectingWriter,
        cache_waiters: CacheWaiters,
        known_derivation_waiters: KnownDerivationWaiters,
        import_waiters: ImportWaiters,
        input_waiters: InputWaiters,
        nar_recv: NarReceiver,
        eval_cache_recv: EvalCacheReceiver,
        eval_shard_replies: EvalShardReplies,
        store: Option<Arc<LocalNixStore>>,
        timeline: Arc<JobTimeline>,
        uploads: UploadClient,
        log: JobLog,
    ) -> Self {
        Self {
            job_id,
            assignment_id,
            writer,
            cache_waiters,
            known_derivation_waiters,
            import_waiters,
            input_waiters,
            nar_recv,
            eval_cache_recv,
            eval_shard_replies,
            store,
            timeline,
            uploads,
            log,
        }
    }

    pub fn phase(&self, phase: JobPhase) -> PhaseGuard {
        self.timeline.enter(phase)
    }

    pub fn concurrent(&self) -> Self {
        Self {
            job_id: self.job_id.clone(),
            assignment_id: self.assignment_id.clone(),
            writer: self.writer.clone(),
            cache_waiters: Arc::clone(&self.cache_waiters),
            known_derivation_waiters: Arc::clone(&self.known_derivation_waiters),
            import_waiters: Arc::clone(&self.import_waiters),
            input_waiters: Arc::clone(&self.input_waiters),
            nar_recv: self.nar_recv.clone(),
            eval_cache_recv: self.eval_cache_recv.clone(),
            eval_shard_replies: self.eval_shard_replies.clone(),
            store: self.store.clone(),
            timeline: self.timeline.concurrent(),
            uploads: self.uploads.clone(),
            log: self.log.clone(),
        }
    }

    pub async fn pull_eval_cache(
        &self,
        fingerprint: &str,
        applied: Vec<String>,
    ) -> Result<PulledSegments> {
        let mut guard = self.phase(JobPhase::EvalCachePull);
        let mut pending = self.eval_cache_recv.register_pull(&self.job_id);
        let session = self
            .writer
            .current_session()
            .context("no session to pull the eval cache over")?;
        self.writer
            .send_now(ClientMessage::EvalCachePull {
                job_id: self.job_id.clone(),
                fingerprint: fingerprint.to_owned(),
                applied,
            })
            .await?;

        let outcome = self
            .within_session(session, pending.await_outcome())
            .await?;
        let (keys, fetch) = match outcome {
            EvalCachePullOutcome::Miss => return Ok(PulledSegments::default()),
            EvalCachePullOutcome::Segments { keys, fetch, .. } => (keys, fetch),
            other => bail!("a segment pull cannot be answered with {other:?}"),
        };

        let segments = self
            .within_session(session, async {
                Ok(fetch_segments(fetch, &mut pending).await)
            })
            .await?;
        let bytes = segments.iter().map(|(_, s)| s.len() as u64).sum();
        guard.record(clamp_to_u32(segments.len()), bytes);
        Ok(PulledSegments { keys, segments })
    }

    async fn within_session<T>(
        &self,
        session: u64,
        answer: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        tokio::select! {
            answered = answer => answered,
            () = self.writer.lost(session) => bail!("the session ended during the eval-cache pull"),
        }
    }

    pub async fn push_eval_cache_segment(&self, fingerprint: &str, bytes: Vec<u8>) -> Result<()> {
        let size_bytes = bytes.len() as u64;
        let mut guard = self.phase(JobPhase::EvalCachePush);
        guard.record(1, size_bytes);
        let object = UploadObject::EvalCacheSegment {
            fingerprint: fingerprint.to_owned(),
        };
        let mut upload = self.uploads.start(&self.job_id, object, size_bytes).await?;
        while let Some((request_id, target)) = upload.next_grant().await? {
            let sent = self
                .send_eval_cache(request_id, &bytes, target)
                .await
                .map(|()| UploadMetadata::EvalCache { size_bytes });
            if upload.settle(sent).await? {
                break;
            }
        }
        Ok(())
    }

    pub async fn announce_eval_source(
        &self,
        store_path: String,
        fingerprint: String,
    ) -> Result<()> {
        self.writer
            .send(ClientMessage::EvalSourceReady {
                job_id: self.job_id.clone(),
                store_path,
                fingerprint,
            })
            .await
    }

    pub async fn announce_eval_shards(&self, shards: Vec<EvalShardSpec>) -> Result<()> {
        self.writer
            .send(ClientMessage::EvalShardsPlanned {
                job_id: self.job_id.clone(),
                shards,
            })
            .await
    }

    pub async fn request_eval_shard(
        &self,
        have_source: bool,
        finished: Option<FinishedShard>,
    ) -> Result<EvalShardOutcome> {
        let replied = self.eval_shard_replies.register(&self.job_id);
        self.writer
            .send(ClientMessage::EvalShardRequest {
                job_id: self.job_id.clone(),
                have_source,
                finished,
            })
            .await?;

        self.eval_shard_replies.wait(&self.job_id, replied).await
    }

    async fn send_eval_cache(
        &self,
        request_id: u64,
        bytes: &[u8],
        target: GrantTarget,
    ) -> Result<()> {
        match target {
            GrantTarget::Passthrough { resume_offset } => {
                passthrough_blob(&self.uploads, request_id, bytes, resume_offset).await
            }
            GrantTarget::Put { url } => {
                gradient_worker_client::object_put::put_object(&url, bytes.to_vec().into(), None)
                    .await
                    .with_context(|| format!("eval-cache PUT {url}"))?;
                Ok(())
            }
            other => bail!("an eval-cache upload cannot use {other:?}"),
        }
    }

    pub async fn query_cache(
        &self,
        paths: Vec<String>,
        mode: QueryMode,
    ) -> Result<Vec<CachedPath>> {
        let mut guard = self.phase(JobPhase::CacheQueryWait);
        guard.record(clamp_to_u32(paths.len()), 0);
        cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            paths,
            Vec::new(),
            mode,
            false,
        )
        .await
    }

    pub async fn query_upstream(&self, path: String) -> Result<Option<CachedPath>> {
        let mut guard = self.phase(JobPhase::CacheQueryWait);
        guard.record(1, 0);
        let answers = cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            vec![path],
            Vec::new(),
            QueryMode::Pull,
            true,
        )
        .await?;

        Ok(answers.into_iter().find(|cp| cp.cached && cp.url.is_some()))
    }

    pub async fn query_push(
        &self,
        paths: Vec<String>,
        sizes: Vec<Option<u64>>,
    ) -> Result<Vec<CachedPath>> {
        let mut nar_sizes = sizes;
        nar_sizes.resize(paths.len(), None);
        if let Some(store) = &self.store {
            let unknown: Vec<usize> = (0..paths.len())
                .filter(|&i| nar_sizes[i].is_none())
                .collect();
            let unknown_paths: Vec<String> = unknown.iter().map(|&i| paths[i].clone()).collect();
            for (i, size) in unknown
                .into_iter()
                .zip(store.nar_sizes(&unknown_paths).await)
            {
                nar_sizes[i] = size;
            }
        }

        let mut guard = self.phase(JobPhase::CacheQueryWait);
        guard.record(clamp_to_u32(paths.len()), 0);
        cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            paths,
            nar_sizes,
            QueryMode::Push,
            false,
        )
        .await
    }

    pub(crate) async fn request_nars(
        &self,
        paths: Vec<String>,
        tally: &Tally,
    ) -> Result<Vec<(String, NarPayload)>> {
        let transfers = paths.into_iter().map(|path| async move {
            let nar = self.shared_nar(&path, tally).await;
            (path, nar)
        });

        let mut out = Vec::new();
        let mut unavailable = Vec::new();
        let mut first_err: Option<anyhow::Error> = None;
        for (path, nar) in futures::future::join_all(transfers).await {
            match nar {
                Ok(Some(body)) => out.push((path, body)),
                Ok(None) => unavailable.push(path),
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
        // A path missing from the cache is a missing input, not a transport failure.
        // A transient error would only spend another attempt on a NAR no retry can produce.
        if !unavailable.is_empty() {
            return Err(anyhow::Error::new(MissingInputs(unavailable)));
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    async fn shared_nar(&self, store_path: &str, tally: &Tally) -> Result<Option<NarPayload>> {
        let mut requested = false;
        let request = async {
            requested = true;
            self.request_nar(store_path, tally.clone()).await
        };
        let downloads = self.nar_recv.shared_downloads();
        let Some(body) = downloads.fetch(store_path, || request).await? else {
            return Ok(None);
        };

        if !requested {
            self.nar_recv
                .discard_partial(&self.job_id, store_path)
                .await;
            tally.count_transfer(body.byte_len().await);
        }
        Ok(Some(body))
    }

    async fn request_nar(&self, store_path: &str, tally: Tally) -> Result<Option<NarPayload>> {
        loop {
            let session = self.writer.attached().await;
            let pending = self.nar_recv.register(&self.job_id, store_path);
            if self
                .writer
                .send_now(self.nar_request(store_path).await)
                .await
                .is_ok()
            {
                let received = pending.received();
                let delivery = count_delivery(
                    tally.clone(),
                    received,
                    self.nar_recv.await_pending(pending),
                );
                tokio::select! {
                    delivered = delivery => return match delivered {
                        Ok(body) => Ok(Some(body)),
                        Err(e) if e.is::<NarUnavailable>() => Ok(None),
                        Err(e) => Err(e),
                    },
                    () = self.writer.lost(session) => {}
                }
            }
            self.nar_recv.interrupt(&self.job_id, store_path).await;
        }
    }

    async fn nar_request(&self, store_path: &str) -> ClientMessage {
        match self.nar_recv.resumable(&self.job_id, store_path).await {
            (received_bytes, Some(stream_token)) if received_bytes > 0 => {
                ClientMessage::NarRequestResume {
                    job_id: self.job_id.clone(),
                    store_path: store_path.to_owned(),
                    received_bytes,
                    stream_token,
                }
            }
            _ => ClientMessage::NarRequest {
                job_id: self.job_id.clone(),
                paths: vec![store_path.to_owned()],
            },
        }
    }

    pub async fn report_fetch_result(&self, flake_source: Option<String>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { flake_source })
            .await
    }

    pub async fn report_evaluating_flake(&self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake).await
    }

    pub async fn report_eval_stats(&self, report: EvalStatsReport) -> Result<()> {
        self.send_update(JobUpdateKind::EvalStats(report)).await
    }

    pub async fn report_building(&self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id }).await
    }

    pub async fn report_build_output(
        &self,
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput {
            build_id,
            outputs,
            metrics,
            substituted,
        })
        .await
    }

    pub(crate) fn build_progress(
        &self,
        build_id: String,
        phase: BuildProgressPhase,
    ) -> Progress<BuildProgressSink> {
        Progress::new(BuildProgressSink {
            writer: self.writer.clone(),
            job_id: self.job_id.clone(),
            assignment_id: self.assignment_id.clone(),
            build_id,
            phase,
        })
    }

    pub async fn report_stage(&self, stage: BuildStage) -> Result<()> {
        self.send_update(JobUpdateKind::Stage(stage)).await
    }

    pub async fn send_eval_message(
        &self,
        level: EvalMessageLevel,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<()> {
        self.writer
            .send(ClientMessage::EvalMessage {
                job_id: self.job_id.clone(),
                level,
                source: source.into(),
                message: message.into(),
            })
            .await
    }

    pub async fn send_log_chunk(&self, data: Vec<u8>) -> Result<()> {
        self.log.send(&self.writer, data).await
    }

    async fn send_update(&self, update: JobUpdateKind) -> Result<()> {
        debug!(job_id = %self.job_id, ?update, "sending job update");
        self.writer
            .send(ClientMessage::JobUpdate {
                job_id: self.job_id.clone(),
                assignment_id: self.assignment_id.get(),
                update,
            })
            .await
    }
}

#[async_trait]
impl JobReporter for JobUpdater {
    fn eval_progress_sink(&self) -> Arc<dyn EvalProgressSink> {
        Arc::new(EvalProgressSender {
            writer: self.writer.clone(),
            job_id: self.job_id.clone(),
            assignment_id: self.assignment_id.clone(),
        })
    }

    async fn query_upstream(&self, path: String) -> Result<Option<CachedPath>> {
        JobUpdater::query_upstream(self, path).await
    }

    async fn query_cache(&self, paths: Vec<String>, mode: QueryMode) -> Result<Vec<CachedPath>> {
        cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            paths,
            Vec::new(),
            mode,
            false,
        )
        .await
    }

    #[tracing::instrument(level = "debug", skip_all, fields(job_id = %self.job_id, paths = drv_paths.len()))]
    async fn query_known_derivations(&self, drv_paths: Vec<String>) -> Result<Vec<String>> {
        let mut guard = self.phase(JobPhase::KnownDerivationsWait);
        guard.record(clamp_to_u32(drv_paths.len()), 0);
        known_derivations_with_timeout(
            &self.job_id,
            &self.writer,
            &self.known_derivation_waiters,
            drv_paths,
        )
        .await
    }

    async fn report_fetching(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Fetching).await
    }

    async fn report_fetch_result(&mut self, flake_source: Option<String>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { flake_source })
            .await
    }

    async fn report_input_update(
        &mut self,
        candidate_lock: String,
        bumped: Vec<gradient_wire::messages::BumpedInputWire>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::InputUpdateResult {
            candidate_lock,
            bumped,
        })
        .await
    }

    async fn report_input_expansion(&mut self, matched: Vec<String>) -> Result<()> {
        self.send_update(JobUpdateKind::InputUpdateExpansion { matched })
            .await
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake).await
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingDerivations).await
    }

    #[tracing::instrument(level = "debug", skip_all, fields(job_id = %self.job_id, derivations = derivations.len()))]
    async fn report_eval_result(
        &self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::EvalResult {
            derivations,
            warnings,
            errors,
        })
        .await
    }

    async fn push_paths(&self, paths: &[(String, Option<u64>)]) -> Result<()> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };

        crate::executor::push_paths(paths, self, &store).await
    }

    async fn pull_paths(&self, paths: Vec<String>) -> Result<()> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };

        crate::proto::prefetch::pull_cached(&store, paths, self).await
    }

    async fn register_realisations(&self, realisations: Vec<Realisation>) -> Result<()> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };

        store.register_realisations(&realisations).await
    }

    #[tracing::instrument(level = "debug", skip_all, fields(job_id = %self.job_id, paths = drv_paths.len()))]
    async fn request_import(&self, drv_paths: Vec<String>) -> Result<ImportOutcome> {
        gradient_worker_client::correlation::request_import(
            &self.job_id,
            &self.writer,
            &self.import_waiters,
            drv_paths,
        )
        .await
    }

    async fn request_input(
        &self,
        request: String,
        progress: Arc<dyn InputProgressSink>,
    ) -> Result<InputOutcome> {
        gradient_worker_client::correlation::request_input(
            &self.job_id,
            &self.writer,
            &self.input_waiters,
            request,
            progress,
        )
        .await
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id }).await
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput {
            build_id,
            outputs,
            metrics,
            substituted,
        })
        .await
    }

    async fn send_log_chunk(&mut self, data: Vec<u8>) -> Result<()> {
        self.log.send(&self.writer, data).await
    }

    async fn send_eval_message(
        &mut self,
        level: EvalMessageLevel,
        source: &str,
        message: &str,
    ) -> Result<()> {
        self.writer
            .send(ClientMessage::EvalMessage {
                job_id: self.job_id.clone(),
                level,
                source: source.to_owned(),
                message: message.to_owned(),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_test_support::prelude::MockProtoServer;
    use gradient_util::sync::Mutex;
    use gradient_wire::messages::CACHE_QUERY_MAX_PATHS;
    use gradient_worker_client::correlation::{deliver_cache_reply, deliver_known_derivations};
    use std::collections::HashMap;

    macro_rules! server_then_client {
        ($job_id:expr, |$sc:ident| $server_body:expr) => {{
            let server = MockProtoServer::bind().await;
            let url = server.url().to_owned();

            let server_task = tokio::spawn(async move {
                let mut $sc = server.accept().await;
                $server_body
            });

            let conn = gradient_worker_client::connection::ProtoConnection::open(&url)
                .await
                .unwrap();
            let job_id: String = $job_id.to_owned();
            (conn, server_task, job_id)
        }};
    }

    fn make_updater(
        job_id: String,
        conn: gradient_worker_client::connection::ProtoConnection,
    ) -> (JobUpdater, gradient_worker_client::connection::ProtoReader) {
        let (writer, reader, _flush) = conn.split();
        let writer = ReconnectingWriter::attached_to(writer);
        let writer_for_uploads = writer.clone();
        let cache_waiters = Arc::new(Mutex::new(HashMap::new()));
        let known_derivation_waiters = Arc::new(Mutex::new(HashMap::new()));
        let import_waiters = Arc::new(Mutex::new(HashMap::new()));
        let input_waiters = Arc::new(Mutex::new(HashMap::new()));
        let nar_recv = NarReceiver::new();
        let eval_cache_recv = EvalCacheReceiver::new();
        let log = JobLog::open(
            &std::env::temp_dir(),
            &job_id,
            crate::proto::job_log::JOB_LOG_BUFFER_CAP,
        );
        let updater = JobUpdater::new(
            job_id,
            AssignmentHandle::new("dispatch-1".to_owned()),
            writer,
            cache_waiters,
            known_derivation_waiters,
            import_waiters,
            input_waiters,
            nar_recv,
            eval_cache_recv,
            Default::default(),
            None,
            JobTimeline::new(),
            UploadClient::new(writer_for_uploads, 8),
            log,
        );
        (updater, reader)
    }

    fn cached(path: &str) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: true,
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

    fn pump_replies(
        mut reader: gradient_worker_client::connection::ProtoReader,
        cache_waiters: CacheWaiters,
        known_waiters: KnownDerivationWaiters,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(msg) = reader.recv().await {
                match msg {
                    gradient_wire::messages::ServerMessage::CacheStatus { query_id, cached } => {
                        deliver_cache_reply(&cache_waiters, &query_id, Ok(cached));
                    }
                    gradient_wire::messages::ServerMessage::KnownDerivations {
                        query_id,
                        known,
                    } => {
                        deliver_known_derivations(&known_waiters, &query_id, known);
                    }
                    _ => {}
                }
            }
        })
    }

    #[tokio::test]
    async fn jobs_requesting_the_same_path_together_transfer_it_once() {
        use gradient_wire::messages::ServerMessage;
        const PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-shared";
        let (conn, server_task, job_id) = server_then_client!("job-a", |sc| {
            let ClientMessage::NarRequest { job_id, paths } = sc.recv().await.unwrap() else {
                panic!("expected a NarRequest");
            };
            assert_eq!(paths, vec![PATH.to_owned()]);
            sc.send(ServerMessage::NarPush {
                job_id,
                store_path: PATH.to_owned(),
                data: Bytes::from_static(b"nar"),
                offset: 0,
                is_final: true,
            })
            .await
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(200), sc.recv())
                .await
                .is_err()
        });

        let (first, mut reader) = make_updater(job_id, conn);
        let mut second = first.concurrent();
        second.job_id = "job-b".to_owned();
        let nars = first.nar_recv.clone();
        let pump = tokio::spawn(async move {
            while let Some(msg) = reader.recv().await {
                nars.absorb(msg).await;
            }
        });

        let tally = Tally::default();
        let (a, b) = tokio::join!(
            first.request_nars(vec![PATH.to_owned()], &tally),
            second.request_nars(vec![PATH.to_owned()], &tally)
        );

        for fetched in [a, b] {
            let (_, body) = fetched.unwrap().pop().unwrap();
            assert_eq!(body.into_bytes().await.unwrap(), b"nar");
        }
        assert!(
            server_task.await.unwrap(),
            "the second job must take the transfer of the first"
        );
        pump.abort();
    }

    #[tokio::test]
    async fn cache_queries_are_pipelined_to_the_window_and_reassembled_in_order() {
        use gradient_wire::messages::{CACHE_QUERY_WINDOW, ServerMessage};
        let chunks = CACHE_QUERY_WINDOW * 2 + 1;
        let total = CACHE_QUERY_MAX_PATHS * (chunks - 1) + 5;
        let (conn, server_task, job_id) = server_then_client!("job-window", |sc| {
            let mut pending: Vec<(String, Vec<String>)> = Vec::new();
            let mut answered = 0usize;
            let mut widest = 0usize;
            while answered < chunks {
                let outstanding = chunks - answered;
                let want = outstanding.min(CACHE_QUERY_WINDOW);
                while pending.len() < want {
                    let msg = sc.recv().await.unwrap();
                    let ClientMessage::CacheQuery {
                        query_id, paths, ..
                    } = msg
                    else {
                        panic!("expected CacheQuery, got {msg:?}");
                    };
                    assert!(paths.len() <= CACHE_QUERY_MAX_PATHS);
                    pending.push((query_id, paths));
                }
                widest = widest.max(pending.len());
                if outstanding > CACHE_QUERY_WINDOW {
                    let extra =
                        tokio::time::timeout(std::time::Duration::from_millis(200), sc.recv())
                            .await;
                    assert!(
                        extra.is_err(),
                        "a query arrived beyond the window: {extra:?}"
                    );
                }
                for (query_id, paths) in pending.drain(..).rev() {
                    let cached = paths.iter().map(|p| cached(p)).collect();
                    sc.send(ServerMessage::CacheStatus { query_id, cached })
                        .await
                        .unwrap();
                    answered += 1;
                }
            }
            widest
        });

        let (updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let paths: Vec<String> = (0..total).map(|i| format!("/nix/store/path-{i}")).collect();
        let got = updater
            .query_cache(paths.clone(), QueryMode::Push)
            .await
            .unwrap();

        assert_eq!(got.into_iter().map(|c| c.path).collect::<Vec<_>>(), paths);
        assert_eq!(
            server_task.await.unwrap(),
            CACHE_QUERY_WINDOW,
            "the window must fill"
        );
        pump.abort();
    }

    #[tokio::test]
    async fn a_push_query_without_a_store_marks_every_size_unknown() {
        use gradient_wire::messages::ServerMessage;
        let (conn, server_task, job_id) = server_then_client!("job-sizes", |sc| {
            let msg = sc.recv().await.unwrap();
            let ClientMessage::CacheQuery {
                query_id,
                paths,
                nar_sizes,
                mode,
                ..
            } = msg
            else {
                panic!("expected a CacheQuery");
            };
            assert_eq!(mode, QueryMode::Push);
            assert_eq!(nar_sizes, vec![None; paths.len()]);
            let cached = paths.iter().map(|p| cached(p)).collect();
            sc.send(ServerMessage::CacheStatus { query_id, cached })
                .await
                .unwrap();
        });

        let (updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let paths: Vec<String> = (0..3).map(|i| format!("/nix/store/path-{i}")).collect();
        let got = updater
            .query_push(paths.clone(), vec![None; paths.len()])
            .await
            .unwrap();

        assert_eq!(got.into_iter().map(|c| c.path).collect::<Vec<_>>(), paths);
        server_task.await.unwrap();
        pump.abort();
    }

    #[tokio::test]
    async fn an_unsized_push_query_still_carries_one_size_per_path() {
        use gradient_wire::messages::ServerMessage;
        let (conn, server_task, job_id) = server_then_client!("job-unsized-push", |sc| {
            let msg = sc.recv().await.unwrap();
            let ClientMessage::CacheQuery {
                query_id,
                paths,
                nar_sizes,
                mode,
                ..
            } = msg
            else {
                panic!("expected a CacheQuery");
            };
            assert_eq!(mode, QueryMode::Push);
            assert_eq!(nar_sizes, vec![None; paths.len()]);
            let cached = paths.iter().map(|p| cached(p)).collect();
            sc.send(ServerMessage::CacheStatus { query_id, cached })
                .await
                .unwrap();
        });

        let (updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let paths: Vec<String> = (0..2).map(|i| format!("/nix/store/path-{i}")).collect();
        let got = updater
            .query_cache(paths.clone(), QueryMode::Push)
            .await
            .unwrap();

        assert_eq!(got.into_iter().map(|c| c.path).collect::<Vec<_>>(), paths);
        server_task.await.unwrap();
        pump.abort();
    }

    #[tokio::test]
    async fn known_derivation_queries_are_pipelined_and_correlate_by_query_id() {
        use gradient_wire::messages::{CACHE_QUERY_WINDOW, ServerMessage};
        let chunks = CACHE_QUERY_WINDOW * 2 + 1;
        let total = CACHE_QUERY_MAX_PATHS * (chunks - 1) + 5;
        let (conn, server_task, job_id) = server_then_client!("job-known-window", |sc| {
            let mut pending: Vec<(String, Vec<String>)> = Vec::new();
            let mut answered = 0usize;
            let mut widest = 0usize;
            while answered < chunks {
                let outstanding = chunks - answered;
                let want = outstanding.min(CACHE_QUERY_WINDOW);
                while pending.len() < want {
                    let msg = sc.recv().await.unwrap();
                    let ClientMessage::QueryKnownDerivations {
                        query_id,
                        drv_paths,
                        ..
                    } = msg
                    else {
                        panic!("expected QueryKnownDerivations, got {msg:?}");
                    };
                    assert!(drv_paths.len() <= CACHE_QUERY_MAX_PATHS);
                    pending.push((query_id, drv_paths));
                }
                widest = widest.max(pending.len());
                if outstanding > CACHE_QUERY_WINDOW {
                    let extra =
                        tokio::time::timeout(std::time::Duration::from_millis(200), sc.recv())
                            .await;
                    assert!(
                        extra.is_err(),
                        "a query arrived beyond the window: {extra:?}"
                    );
                }
                for (query_id, drv_paths) in pending.drain(..).rev() {
                    sc.send(ServerMessage::KnownDerivations {
                        query_id,
                        known: drv_paths,
                    })
                    .await
                    .unwrap();
                    answered += 1;
                }
            }
            widest
        });

        let (updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let drvs: Vec<String> = (0..total)
            .map(|i| format!("/nix/store/d-{i}.drv"))
            .collect();
        let got = JobReporter::query_known_derivations(&updater, drvs.clone())
            .await
            .unwrap();

        assert_eq!(got, drvs);
        assert_eq!(
            server_task.await.unwrap(),
            CACHE_QUERY_WINDOW,
            "the window must fill"
        );
        pump.abort();
    }

    struct NoProgress;

    impl InputProgressSink for NoProgress {
        fn transfer(&self, _bytes_done: u64, _bytes_total: Option<u64>) {}
    }

    #[tokio::test]
    async fn request_input_sends_an_input_request_and_returns_the_result() {
        use gradient_wire::messages::ServerMessage;
        const REQUEST: &str =
            r#"{"builtin":"fetchTree","args":{"type":"tarball","url":"https://x"}}"#;
        let (conn, server_task, job_id) = server_then_client!("job-input", |sc| {
            let ClientMessage::InputRequest {
                job_id,
                request_id,
                request,
            } = sc.recv().await.unwrap()
            else {
                panic!("expected an InputRequest");
            };
            assert_eq!((job_id.as_str(), request.as_str()), ("job-input", REQUEST));
            sc.send(ServerMessage::InputResult {
                job_id,
                request_id,
                outcome: InputOutcome::Fetched {
                    response: r#"{"storePath":"/nix/store/aaaa-source"}"#.into(),
                },
            })
            .await
            .unwrap();
        });

        let (updater, mut reader) = make_updater(job_id, conn);
        let waiters = Arc::clone(&updater.input_waiters);
        let pump = tokio::spawn(async move {
            while let Some(msg) = reader.recv().await {
                if let ServerMessage::InputResult {
                    request_id,
                    outcome,
                    ..
                } = msg
                {
                    gradient_worker_client::correlation::deliver_input_result(
                        &waiters,
                        &request_id,
                        outcome,
                    );
                }
            }
        });

        let outcome = updater
            .request_input(REQUEST.into(), Arc::new(NoProgress))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InputOutcome::Fetched {
                response: r#"{"storePath":"/nix/store/aaaa-source"}"#.into()
            }
        );
        server_task.await.unwrap();
        pump.abort();
    }
}
