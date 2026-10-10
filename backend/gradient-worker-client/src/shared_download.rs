/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use gradient_util::sync::Mutex;

use crate::nar_recv::{NarPayload, WeakNarPayload};

#[derive(Default)]
struct Slot {
    body: tokio::sync::Mutex<Option<WeakNarPayload>>,
    failure: Mutex<Failure>,
}

#[derive(Default, Clone)]
struct Failure {
    count: u64,
    reason: String,
}

/// Jobs starting together miss the same inputs. Later jobs wait for the first transfer of a store
/// path and take a clone of its body. Jobs waiting on a failed transfer fail with it, and a
/// dropped transfer leaves the path to the next waiting job.
#[derive(Clone, Default)]
pub struct SharedDownloads {
    slots: Arc<Mutex<HashMap<String, Arc<Slot>>>>,
}

impl SharedDownloads {
    fn slot(&self, store_path: &str) -> Arc<Slot> {
        let mut slots = self.slots.lock();
        slots.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || slot.body.try_lock().is_ok_and(|body| {
                    body.as_ref()
                        .is_some_and(|shared| shared.upgrade().is_some())
                })
        });
        Arc::clone(slots.entry(store_path.to_owned()).or_default())
    }

    pub async fn fetch<F, Fut>(&self, store_path: &str, download: F) -> Result<Option<NarPayload>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<NarPayload>>>,
    {
        let slot = self.slot(store_path);
        let failures_before = slot.failure.lock().count;
        let mut body = slot.body.lock().await;
        if let Some(shared) = body.as_ref().and_then(WeakNarPayload::upgrade) {
            return Ok(Some(shared));
        }

        let failure = slot.failure.lock().clone();
        anyhow::ensure!(
            failure.count == failures_before,
            "the shared transfer of {store_path} failed: {}",
            failure.reason
        );

        let downloaded = download().await.inspect_err(|e| {
            let mut failure = slot.failure.lock();
            failure.count += 1;
            failure.reason = format!("{e:#}");
        })?;
        *body = downloaded.as_ref().map(NarPayload::downgrade);
        Ok(downloaded)
    }

    pub fn evict(&self, store_path: &str) {
        self.slots.lock().remove(store_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello";

    fn nar() -> Result<Option<NarPayload>> {
        Ok(Some(b"nar".to_vec().into()))
    }

    #[tokio::test]
    async fn jobs_fetching_the_same_path_together_download_it_once() {
        let downloads = SharedDownloads::default();
        let calls = AtomicUsize::new(0);
        let download = || async {
            calls.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            nar()
        };

        let (first, second) = tokio::join!(
            downloads.fetch(PATH, download),
            downloads.fetch(PATH, download)
        );

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        for body in [first, second] {
            assert_eq!(body.unwrap().unwrap().into_bytes().await.unwrap(), b"nar");
        }
    }

    #[tokio::test]
    async fn jobs_waiting_on_a_failed_transfer_fail_with_it() {
        let downloads = SharedDownloads::default();
        let failing = || async {
            tokio::task::yield_now().await;
            anyhow::bail!("connection reset")
        };
        let waiting = || async { anyhow::bail!("a waiting job must not transfer again") };

        let (first, second) = tokio::join!(
            downloads.fetch(PATH, failing),
            downloads.fetch(PATH, waiting)
        );

        assert!(first.is_err());
        let shared = format!("{:#}", second.unwrap_err());
        assert!(shared.contains("connection reset"), "{shared}");
    }

    #[tokio::test]
    async fn a_failed_download_leaves_the_path_to_a_later_job() {
        let downloads = SharedDownloads::default();
        let failed = downloads
            .fetch(PATH, || async { anyhow::bail!("connection reset") })
            .await;
        assert!(failed.is_err());

        let retried = downloads.fetch(PATH, || async { nar() }).await;
        assert!(retried.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_path_is_downloaded_again_once_no_job_holds_its_body() {
        let downloads = SharedDownloads::default();
        let calls = AtomicUsize::new(0);
        let download = || async {
            calls.fetch_add(1, Ordering::Relaxed);
            nar()
        };

        drop(downloads.fetch(PATH, download).await.unwrap());
        drop(downloads.fetch(PATH, download).await.unwrap());

        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn an_evicted_body_is_downloaded_again() {
        let downloads = SharedDownloads::default();
        let calls = AtomicUsize::new(0);
        let download = || async {
            calls.fetch_add(1, Ordering::Relaxed);
            nar()
        };

        let corrupt = downloads.fetch(PATH, download).await.unwrap();
        downloads.evict(PATH);
        let fresh = downloads.fetch(PATH, download).await.unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        drop((corrupt, fresh));
    }
}
