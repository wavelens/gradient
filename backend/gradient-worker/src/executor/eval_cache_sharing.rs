/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gradient_storage::eval_cache_segment::decode;
use gradient_util::sync::Mutex;
use tokio::sync::oneshot;
use tracing::warn;

use super::eval_cache_sync::{self, EvalCacheFile, is_busy};
use crate::proto::job::{JobUpdater, PulledSegments};

pub const EVAL_CACHE_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

type SharedEvalCacheFile = Arc<Mutex<EvalCacheFile>>;

pub(super) async fn share_eval_cache(
    path: PathBuf,
    updater: &JobUpdater,
    fingerprint: String,
) -> Option<EvalCacheSharing> {
    let file = match blocking(move || eval_cache_sync::open(&path)).await {
        Ok(file) => Arc::new(Mutex::new(file)),
        Err(e) => {
            warn!(error = %e, "opening the eval cache failed; evaluating local-only");
            return None;
        }
    };

    pull_segments(&file, updater, &fingerprint).await;
    let upload = SegmentUpload {
        updater: updater.concurrent(),
        fingerprint,
    };
    Some(EvalCacheSharing::new(file, Arc::new(upload)))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .context("eval cache task panicked")?
}

async fn on_file<T: Send + 'static>(
    file: &SharedEvalCacheFile,
    work: impl FnOnce(&mut EvalCacheFile) -> Result<T> + Send + 'static,
) -> Result<T> {
    let file = Arc::clone(file);
    blocking(move || work(&mut file.lock())).await
}

async fn pull_segments(file: &SharedEvalCacheFile, updater: &JobUpdater, fingerprint: &str) {
    let applied = match on_file(file, |file| file.applied_keys()).await {
        Ok(keys) => keys.into_iter().collect(),
        Err(e) => {
            warn!(error = %e, "reading the applied eval cache segments failed; evaluating local-only");
            return;
        }
    };

    let pulled = match updater.pull_eval_cache(fingerprint, applied).await {
        Ok(pulled) => pulled,
        Err(e) => {
            warn!(error = %e, "eval-cache pull failed; evaluating local-only");
            return;
        }
    };

    if let Err(e) = on_file(file, move |file| apply_pulled(file, pulled)).await {
        warn!(error = %e, "applying the pulled eval cache segments failed");
    }
}

fn apply_pulled(file: &mut EvalCacheFile, pulled: PulledSegments) -> Result<()> {
    for (key, bytes) in &pulled.segments {
        match decode(bytes).and_then(|rows| file.apply(key, &rows)) {
            Ok(()) => {}
            Err(e) if is_busy(&e) => {
                warn!(error = %e, "the eval cache stayed locked; the remaining segments are pulled again next time");
                return Ok(());
            }
            Err(e) => warn!(%key, error = %e, "skipping an eval cache segment"),
        }
    }

    file.forget_keys_except(&pulled.keys)
}

#[async_trait::async_trait]
trait SegmentSink: Send + Sync {
    async fn push(&self, segment: Vec<u8>) -> Result<()>;
}

struct SegmentUpload {
    updater: JobUpdater,
    fingerprint: String,
}

#[async_trait::async_trait]
impl SegmentSink for SegmentUpload {
    async fn push(&self, segment: Vec<u8>) -> Result<()> {
        self.updater
            .push_eval_cache_segment(&self.fingerprint, segment)
            .await
    }
}

pub(super) struct EvalCacheSharing {
    file: SharedEvalCacheFile,
    sink: Arc<dyn SegmentSink>,
}

impl EvalCacheSharing {
    fn new(file: SharedEvalCacheFile, sink: Arc<dyn SegmentSink>) -> Self {
        Self { file, sink }
    }

    pub(super) async fn beside_flusher<T>(
        &mut self,
        interval: Duration,
        evaluation: impl Future<Output = T>,
    ) -> T {
        let (stop, stopped) = oneshot::channel::<()>();
        let evaluation = async move {
            let out = evaluation.await;
            drop(stop);
            out
        };
        let (out, ()) = tokio::join!(evaluation, self.flush_every(interval, stopped));
        out
    }

    async fn flush_every(&mut self, interval: Duration, mut stopped: oneshot::Receiver<()>) {
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => return,
                () = tokio::time::sleep(interval) => self.flush().await,
            }
        }
    }

    pub(super) async fn flush(&mut self) {
        let mut unsent = on_file(&self.file, |file| file.unsent_segments())
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "reading the unsent eval cache segments failed");
                Vec::new()
            });
        match on_file(&self.file, EvalCacheFile::export_changes).await {
            Ok(exported) => unsent.extend(exported),
            Err(e) => warn!(error = %e, "eval cache export failed; trying again at the next flush"),
        }

        for (id, segment) in unsent {
            if let Err(e) = self.sink.push(segment).await {
                warn!(error = %e, "eval cache segment push failed; trying again at the next flush");
                continue;
            }

            if let Err(e) = on_file(&self.file, move |file| file.mark_sent(id)).await {
                warn!(error = %e, "marking an eval cache segment as sent failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::eval_cache_segment::{ATTR_FULL_ATTRS, EvalCacheRow, encode};
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn cache_row(path: &[&str]) -> EvalCacheRow {
        EvalCacheRow {
            path: path.iter().map(|part| (*part).to_owned()).collect(),
            kind: ATTR_FULL_ATTRS,
            value: None,
            context: None,
        }
    }

    fn pulled(segments: Vec<(&str, Vec<u8>)>) -> PulledSegments {
        PulledSegments {
            keys: segments.iter().map(|(key, _)| (*key).to_owned()).collect(),
            segments: segments
                .into_iter()
                .map(|(key, bytes)| (key.to_owned(), bytes))
                .collect(),
        }
    }

    fn nix(path: &Path) -> rusqlite::Connection {
        let nix = rusqlite::Connection::open(path).unwrap();
        nix.busy_timeout(Duration::from_secs(30)).unwrap();
        nix
    }

    fn nix_write(nix: &rusqlite::Connection, parent: i64, name: &str) -> i64 {
        nix.execute(
            "insert or replace into Attributes(parent, name, type, value) values (?, ?, ?, ?)",
            rusqlite::params![parent, name, ATTR_FULL_ATTRS, None::<String>],
        )
        .unwrap();
        nix.last_insert_rowid()
    }

    fn pushed_paths(pushed: &[Vec<u8>]) -> Vec<Vec<String>> {
        let mut paths: Vec<Vec<String>> = pushed
            .iter()
            .flat_map(|segment| decode(segment).unwrap())
            .map(|row| row.path)
            .collect();
        paths.sort();
        paths
    }

    #[derive(Default)]
    struct RecordingSink {
        failures_left: AtomicUsize,
        pushed: Mutex<Vec<Vec<u8>>>,
    }

    impl RecordingSink {
        fn failing(times: usize) -> Self {
            Self {
                failures_left: AtomicUsize::new(times),
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl SegmentSink for RecordingSink {
        async fn push(&self, segment: Vec<u8>) -> Result<()> {
            let fail = self
                .failures_left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if fail {
                anyhow::bail!("the server is unreachable");
            }

            self.pushed.lock().push(segment);
            Ok(())
        }
    }

    #[test]
    fn a_bad_segment_is_skipped_and_not_marked_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = eval_cache_sync::open(&dir.path().join("fp.sqlite")).unwrap();
        let good = encode(&[cache_row(&["a"])]).unwrap();
        let mut bad = encode(&[cache_row(&["b"])]).unwrap();
        bad.truncate(bad.len() / 2);

        apply_pulled(&mut file, pulled(vec![("good", good), ("bad", bad)])).unwrap();

        assert_eq!(
            file.applied_keys().unwrap(),
            HashSet::from(["good".to_owned()])
        );
    }

    #[test]
    fn a_locked_cache_stops_applying_at_the_first_busy_segment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fp.sqlite");
        let mut file = eval_cache_sync::open(&path).unwrap();
        file.set_busy_timeout(Duration::from_secs(1));
        let mut writer = nix(&path);
        let lock = writer
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let segments = vec![
            ("first", encode(&[cache_row(&["a"])]).unwrap()),
            ("second", encode(&[cache_row(&["b"])]).unwrap()),
        ];
        let started = Instant::now();

        apply_pulled(&mut file, pulled(segments)).unwrap();

        assert!(started.elapsed() < Duration::from_millis(2500));
        drop(lock);
        assert!(file.applied_keys().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_final_flush_pushes_rows_written_after_the_last_tick() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fp.sqlite");
        let file = Arc::new(Mutex::new(eval_cache_sync::open(&path).unwrap()));
        let sink = Arc::new(RecordingSink::default());
        let mut sharing = EvalCacheSharing::new(file, sink.clone());
        let nix = nix(&path);
        let root = nix_write(&nix, 0, "");

        sharing
            .beside_flusher(Duration::from_millis(50), async {
                nix_write(&nix, root, "before");
                tokio::time::sleep(Duration::from_millis(150)).await;
                nix_write(&nix, root, "after");
            })
            .await;
        sharing.flush().await;

        assert_eq!(
            pushed_paths(&sink.pushed.lock()),
            vec![
                Vec::<String>::new(),
                vec!["after".to_owned()],
                vec!["before".to_owned()],
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_push_is_sent_by_the_next_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fp.sqlite");
        let file = Arc::new(Mutex::new(eval_cache_sync::open(&path).unwrap()));
        let sink = Arc::new(RecordingSink::failing(1));
        let mut sharing = EvalCacheSharing::new(file, sink.clone());
        let nix = nix(&path);
        nix_write(&nix, 0, "");

        sharing.flush().await;
        assert!(sink.pushed.lock().is_empty());
        sharing.flush().await;

        assert_eq!(
            pushed_paths(&sink.pushed.lock()),
            vec![Vec::<String>::new()]
        );
        assert!(
            eval_cache_sync::open(&path)
                .unwrap()
                .unsent_segments()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn segments_whose_push_failed_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fp.sqlite");
        let file = Arc::new(Mutex::new(eval_cache_sync::open(&path).unwrap()));
        let mut sharing = EvalCacheSharing::new(file, Arc::new(RecordingSink::failing(usize::MAX)));
        let nix = nix(&path);
        nix_write(&nix, 0, "");

        sharing.flush().await;
        drop(sharing);

        let reopened = eval_cache_sync::open(&path).unwrap();
        let unsent: Vec<Vec<u8>> = reopened
            .unsent_segments()
            .unwrap()
            .into_iter()
            .map(|(_, segment)| segment)
            .collect();
        assert_eq!(pushed_paths(&unsent), vec![Vec::<String>::new()]);
    }
}
