/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Instant;

use gradient_wire::messages::WORKER_LOCAL_STORE_BLOOM_FILTER_REBUILD_AND_RESEND_INTERVAL;
use gradient_wire::traits::WorkerStore;
use gradient_wire::worker_store_path_bloom_filter::{
    BloomFilterBitArrayChunk, FILTER_BITS_PER_STORE_PATH,
    MAX_WORKER_STORE_PATH_BLOOM_FILTER_BIT_COUNT, WorkerStorePathBloomFilter,
};
use gradient_worker_client::connection::ProtoWriter;
use tokio_util::task::AbortOnDropHandle;
use tracing::{debug, warn};

pub(super) async fn build_local_store_bloom_filter_chunks<S: WorkerStore + ?Sized>(
    store: &S,
) -> Option<Vec<BloomFilterBitArrayChunk>> {
    let started = Instant::now();
    let hashes = store
        .local_store_valid_path_hashes()
        .await
        .inspect_err(|e| warn!(error = %e, "Failed to get the list of local store paths. No Bloom filter for the server this time."))
        .ok()?;
    let path_count = hashes.len();
    if !store_path_bloom_filter_fits_max_bit_count(path_count) {
        warn!(
            path_count,
            "The local store has too many paths for the server's Bloom filter limit. This worker doesn't send a Bloom filter for store paths."
        );
        return None;
    }
    let chunks = WorkerStorePathBloomFilter::from_store_path_hash_parts(&hashes)
        .into_wire_chunks_of_at_most_max_chunk_size_with_last_final();
    debug!(
        path_count,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "Built the Bloom filter for the local store."
    );
    Some(chunks)
}

fn store_path_bloom_filter_fits_max_bit_count(path_count: usize) -> bool {
    u64::try_from(path_count.max(1))
        .ok()
        .and_then(|count| count.checked_mul(FILTER_BITS_PER_STORE_PATH))
        .is_some_and(|bits| bits <= MAX_WORKER_STORE_PATH_BLOOM_FILTER_BIT_COUNT)
}

fn spawn_local_store_bloom_filter_rebuild_and_send_chunks_over_connection_writer<
    S: WorkerStore + ?Sized + 'static,
>(
    store: Arc<S>,
    writer: ProtoWriter,
) -> AbortOnDropHandle<()> {
    #[expect(
        clippy::disallowed_methods,
        reason = "Send through the writer. End the task on connection loss."
    )]
    let task = tokio::spawn(async move {
        let Some(chunks) = build_local_store_bloom_filter_chunks(&*store).await else {
            return;
        };
        for chunk in chunks {
            if let Err(e) = writer.send(chunk.into()).await {
                warn!(error = %e, "Failed to send the Bloom filter for the local store");
                return;
            }
        }
    });
    AbortOnDropHandle::new(task)
}

pub(super) struct BloomFilterRebuildLastStartAndRunningTaskHandle {
    last_store_bloom_filter_rebuild_started_at: Instant,
    store_bloom_filter_rebuild_task_join_handle: Option<AbortOnDropHandle<()>>,
}

impl BloomFilterRebuildLastStartAndRunningTaskHandle {
    pub(super) fn new() -> Self {
        Self {
            last_store_bloom_filter_rebuild_started_at: Instant::now(),
            store_bloom_filter_rebuild_task_join_handle: None,
        }
    }

    pub(super) fn start_rebuild_and_send_if_due_and_not_running<
        S: WorkerStore + ?Sized + 'static,
    >(
        &mut self,
        store: &Arc<S>,
        writer: &ProtoWriter,
    ) {
        let due = self.last_store_bloom_filter_rebuild_started_at.elapsed()
            >= WORKER_LOCAL_STORE_BLOOM_FILTER_REBUILD_AND_RESEND_INTERVAL;
        let running = self
            .store_bloom_filter_rebuild_task_join_handle
            .as_ref()
            .is_some_and(|task| !task.is_finished());
        if !due || running {
            return;
        }
        self.last_store_bloom_filter_rebuild_started_at = Instant::now();
        self.store_bloom_filter_rebuild_task_join_handle = Some(
            spawn_local_store_bloom_filter_rebuild_and_send_chunks_over_connection_writer(
                Arc::clone(store),
                writer.clone(),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_path_count_filter_fits_server_maximum_bit_count_is_sent_one_more_is_not() {
        let largest_accepted = usize::try_from(
            MAX_WORKER_STORE_PATH_BLOOM_FILTER_BIT_COUNT / FILTER_BITS_PER_STORE_PATH,
        )
        .unwrap();

        assert!(store_path_bloom_filter_fits_max_bit_count(0));
        assert!(store_path_bloom_filter_fits_max_bit_count(largest_accepted));
        assert!(!store_path_bloom_filter_fits_max_bit_count(
            largest_accepted + 1
        ));
    }
}
