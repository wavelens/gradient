/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A call whose future is dropped before it finished is counted as cancelled. A hung backend is
//! showing up this way.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use gradient_util::telemetry::{Gauges, MinuteStats, metric};
use object_store::path::Path;
use object_store::{
    CopyOptions, GetOptions, GetResult, GetResultPayload, ListResult, MultipartUpload, ObjectMeta,
    ObjectStore, PutMultipartOptions, PutOptions, PutPayload, PutResult, RenameOptions, Result,
    UploadPart,
};

pub(crate) struct TimedStore {
    inner: Arc<dyn ObjectStore>,
    stats: &'static MinuteStats,
    gauges: &'static Gauges,
}

impl TimedStore {
    pub(crate) fn new(
        inner: Arc<dyn ObjectStore>,
        stats: &'static MinuteStats,
        gauges: &'static Gauges,
    ) -> Self {
        Self {
            inner,
            stats,
            gauges,
        }
    }
}

impl fmt::Debug for TimedStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TimedStore({:?})", self.inner)
    }
}

impl fmt::Display for TimedStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TimedStore({})", self.inner)
    }
}

pub(crate) struct OpGuard {
    stats: &'static MinuteStats,
    op: &'static str,
    started: Instant,
    done: bool,
}

impl OpGuard {
    pub(crate) fn start(stats: &'static MinuteStats, op: &'static str) -> Self {
        Self {
            stats,
            op,
            started: Instant::now(),
            done: false,
        }
    }

    fn finish<T>(self, result: Result<T>) -> Result<T> {
        self.finish_with(matches!(
            result,
            Ok(_) | Err(object_store::Error::NotFound { .. })
        ));
        result
    }

    pub(crate) fn finish_with(mut self, answered: bool) {
        self.done = true;
        if answered {
            let ms = self.started.elapsed().as_secs_f64() * 1000.0;
            self.stats.record(metric::STORAGE_OP_MS, self.op, ms);
        } else {
            self.stats
                .record(metric::STORAGE_OP_ERRORS, format!("{}/error", self.op), 1.0);
        }
    }
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        if !self.done {
            self.stats.record(
                metric::STORAGE_OP_ERRORS,
                format!("{}/cancelled", self.op),
                1.0,
            );
        }
    }
}

pub(crate) fn watch_stream<E: Send + 'static>(
    stream: BoxStream<'static, Result<Bytes, E>>,
    stats: &'static MinuteStats,
    gauges: &'static Gauges,
) -> BoxStream<'static, Result<Bytes, E>> {
    let reading = gauges.storage_reads.enter();
    stream
        .inspect(move |item| {
            let _reading = &reading;
            match item {
                Ok(chunk) => stats.record(metric::STORAGE_READ_BYTES, "", chunk.len() as f64),
                Err(_) => stats.record(metric::STORAGE_OP_ERRORS, "read/error", 1.0),
            }
        })
        .boxed()
}

struct TimedUpload {
    inner: Box<dyn MultipartUpload>,
    stats: &'static MinuteStats,
    gauges: &'static Gauges,
}

impl fmt::Debug for TimedUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TimedUpload({:?})", self.inner)
    }
}

#[async_trait]
impl MultipartUpload for TimedUpload {
    fn put_part(&mut self, data: PutPayload) -> UploadPart {
        let guard = OpGuard::start(self.stats, "put_part");
        self.stats.record(
            metric::STORAGE_WRITE_BYTES,
            "",
            data.content_length() as f64,
        );
        let writing = self.gauges.storage_writes.enter();
        let part = self.inner.put_part(data);
        Box::pin(async move {
            let _writing = writing;
            guard.finish(part.await)
        })
    }

    async fn complete(&mut self) -> Result<PutResult> {
        let guard = OpGuard::start(self.stats, "multipart_complete");
        guard.finish(self.inner.complete().await)
    }

    async fn abort(&mut self) -> Result<()> {
        self.inner.abort().await
    }
}

#[async_trait]
impl ObjectStore for TimedStore {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        opts: PutOptions,
    ) -> Result<PutResult> {
        let guard = OpGuard::start(self.stats, "put");
        self.stats.record(
            metric::STORAGE_WRITE_BYTES,
            "",
            payload.content_length() as f64,
        );
        let _writing = self.gauges.storage_writes.enter();
        guard.finish(self.inner.put_opts(location, payload, opts).await)
    }

    async fn put_multipart_opts(
        &self,
        location: &Path,
        opts: PutMultipartOptions,
    ) -> Result<Box<dyn MultipartUpload>> {
        let guard = OpGuard::start(self.stats, "multipart_create");
        let inner = guard.finish(self.inner.put_multipart_opts(location, opts).await)?;
        Ok(Box::new(TimedUpload {
            inner,
            stats: self.stats,
            gauges: self.gauges,
        }))
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> Result<GetResult> {
        let op = if options.head { "head" } else { "get" };
        let guard = OpGuard::start(self.stats, op);
        let mut result = guard.finish(self.inner.get_opts(location, options).await)?;

        if let GetResultPayload::Stream(stream) = result.payload {
            result.payload =
                GetResultPayload::Stream(watch_stream(stream, self.stats, self.gauges));
        }

        Ok(result)
    }

    async fn get_ranges(&self, location: &Path, ranges: &[Range<u64>]) -> Result<Vec<Bytes>> {
        let guard = OpGuard::start(self.stats, "get");
        guard.finish(self.inner.get_ranges(location, ranges).await)
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, Result<Path>>,
    ) -> BoxStream<'static, Result<Path>> {
        let stats = self.stats;
        let started = Instant::now();
        self.inner
            .delete_stream(locations)
            .inspect(move |item| match item {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => stats.record(
                    metric::STORAGE_OP_MS,
                    "delete",
                    started.elapsed().as_secs_f64() * 1000.0,
                ),
                Err(_) => stats.record(metric::STORAGE_OP_ERRORS, "delete/error", 1.0),
            })
            .boxed()
    }

    fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
        self.inner.list(prefix)
    }

    fn list_with_offset(
        &self,
        prefix: Option<&Path>,
        offset: &Path,
    ) -> BoxStream<'static, Result<ObjectMeta>> {
        self.inner.list_with_offset(prefix, offset)
    }

    async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> Result<()> {
        self.inner.copy_opts(from, to, options).await
    }

    async fn rename_opts(&self, from: &Path, to: &Path, options: RenameOptions) -> Result<()> {
        self.inner.rename_opts(from, to, options).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use object_store::memory::InMemory;
    use object_store::{ObjectStoreExt, PutPayload};

    fn timed() -> (TimedStore, &'static MinuteStats) {
        let stats: &'static MinuteStats = Box::leak(Box::default());
        let gauges: &'static Gauges = Box::leak(Box::default());
        (
            TimedStore::new(Arc::new(InMemory::new()), stats, gauges),
            stats,
        )
    }

    fn sum(stats: &MinuteStats, metric: &str) -> f64 {
        stats
            .snapshot()
            .iter()
            .filter(|(k, _)| k.metric == metric)
            .map(|(_, a)| a.sum)
            .sum()
    }

    fn count(stats: &MinuteStats, metric: &str, label: &str) -> i64 {
        stats
            .snapshot()
            .iter()
            .filter(|(k, _)| k.metric == metric && k.label == label)
            .map(|(_, a)| a.count)
            .sum()
    }

    #[tokio::test]
    async fn successful_calls_record_their_duration() {
        let (store, stats) = timed();
        let path = Path::from("a");
        store
            .put(&path, PutPayload::from_static(b"x"))
            .await
            .expect("put");
        store.head(&path).await.expect("head");
        store.get(&path).await.expect("get");

        assert_eq!(count(stats, metric::STORAGE_OP_MS, "put"), 1);
        assert_eq!(count(stats, metric::STORAGE_OP_MS, "head"), 1);
        assert_eq!(count(stats, metric::STORAGE_OP_MS, "get"), 1);
    }

    #[tokio::test]
    async fn written_and_streamed_bytes_are_counted() {
        let (store, stats) = timed();
        let path = Path::from("a");
        store
            .put(&path, PutPayload::from_static(b"nar"))
            .await
            .expect("put");
        let read: Vec<_> = store
            .get(&path)
            .await
            .expect("get")
            .into_stream()
            .collect()
            .await;

        assert_eq!(read.len(), 1);
        assert_eq!(sum(stats, metric::STORAGE_WRITE_BYTES), 3.0);
        assert_eq!(sum(stats, metric::STORAGE_READ_BYTES), 3.0);
    }

    #[tokio::test]
    async fn a_failed_call_counts_as_error() {
        let (store, stats) = timed();
        let path = Path::from("a");
        let create = || PutOptions::from(object_store::PutMode::Create);
        store
            .put_opts(&path, PutPayload::from_static(b"x"), create())
            .await
            .expect("first put");
        let _ = store
            .put_opts(&path, PutPayload::from_static(b"x"), create())
            .await;

        assert_eq!(count(stats, metric::STORAGE_OP_ERRORS, "put/error"), 1);
        assert_eq!(count(stats, metric::STORAGE_OP_MS, "put"), 1);
    }

    #[tokio::test]
    async fn a_missing_object_is_an_answer_not_an_error() {
        let (store, stats) = timed();
        let _ = store.get(&Path::from("missing")).await;

        assert_eq!(count(stats, metric::STORAGE_OP_ERRORS, "get/error"), 0);
        assert_eq!(count(stats, metric::STORAGE_OP_MS, "get"), 1);
    }

    #[tokio::test]
    async fn a_dropped_call_counts_as_cancelled() {
        let (_store, stats) = timed();
        let guard = OpGuard::start(stats, "get");
        drop(guard);

        assert_eq!(count(stats, metric::STORAGE_OP_ERRORS, "get/cancelled"), 1);
    }

    #[tokio::test]
    async fn a_stream_error_counts_as_read_error() {
        let stats: &'static MinuteStats = Box::leak(Box::default());
        let failing = futures::stream::iter(vec![Err(object_store::Error::Generic {
            store: "test",
            source: "boom".into(),
        })])
        .boxed();

        let mut stream = watch_stream(failing, stats, Box::leak(Box::default()));
        assert!(stream.next().await.expect("item").is_err());
        assert_eq!(count(stats, metric::STORAGE_OP_ERRORS, "read/error"), 1);
    }

    #[tokio::test]
    async fn a_dropped_part_upload_counts_as_cancelled() {
        let (store, stats) = timed();
        let mut upload = store
            .put_multipart(&Path::from("a"))
            .await
            .expect("multipart");

        drop(upload.put_part(PutPayload::from_static(b"x")));

        assert_eq!(count(stats, metric::STORAGE_OP_MS, "multipart_create"), 1);
        assert_eq!(
            count(stats, metric::STORAGE_OP_ERRORS, "put_part/cancelled"),
            1
        );
    }

    #[tokio::test]
    async fn delete_is_timed_per_path() {
        let (store, stats) = timed();
        let path = Path::from("a");
        store
            .put(&path, PutPayload::from_static(b"x"))
            .await
            .expect("put");
        store.delete(&path).await.expect("delete");

        assert_eq!(count(stats, metric::STORAGE_OP_MS, "delete"), 1);
    }
}
