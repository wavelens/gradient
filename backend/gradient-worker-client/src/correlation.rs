/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Correlates `CacheStatus` / `KnownDerivations` replies with the query that
//! asked, by `query_id`, so a job may keep several queries in flight.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use futures::{StreamExt as _, TryStreamExt as _};
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    CACHE_QUERY_MAX_PATHS, CACHE_QUERY_TIMEOUT, CACHE_QUERY_WINDOW, CachedPath, ClientMessage,
    QueryMode,
};
use tokio::sync::oneshot;

use crate::connection::ProtoWriter;

/// A pending `CacheQuery`: its reply channel plus the owning `job_id` so a
/// finished or aborted job can drop any query it left in flight.
pub struct CacheWaiter {
    job_id: String,
    reply: oneshot::Sender<Result<Vec<CachedPath>, String>>,
}

/// Shared map from a unique per-query id to its pending `CacheQuery`.
/// Correlating by the query id (not the `job_id`) lets one job keep several
/// CacheQueries in flight - and survive retries that reuse the `job_id` -
/// without their replies colliding: a stale or out-of-order reply reaches the
/// exact waiter that sent it, or none. `Ok` carries a `CacheStatus`,
/// `Err(message)` a server-side `CacheError` (indeterminate - retry, never
/// "inputs absent").
pub type CacheWaiters = Arc<Mutex<HashMap<String, CacheWaiter>>>;

/// Register a oneshot for `query_id` (scoped to `job_id`) and hand back its
/// receiver.
pub fn register_cache_waiter(
    waiters: &CacheWaiters,
    query_id: String,
    job_id: String,
) -> oneshot::Receiver<Result<Vec<CachedPath>, String>> {
    let (reply, rx) = oneshot::channel();
    waiters
        .lock()
        .insert(query_id, CacheWaiter { job_id, reply });
    rx
}

/// Deliver a `CacheStatus`/`CacheError` to the waiter that sent `query_id`.
/// Returns false (dropping the reply) when no waiter is registered - a late
/// reply for an already-timed-out or superseded query.
pub fn deliver_cache_reply(
    waiters: &CacheWaiters,
    query_id: &str,
    result: Result<Vec<CachedPath>, String>,
) -> bool {
    match waiters.lock().remove(query_id) {
        Some(w) => {
            let _ = w.reply.send(result);
            true
        }
        None => false,
    }
}

/// Drop the waiter for a single timed-out query so a late reply is discarded
/// rather than delivered to a closed channel.
pub fn forget_cache_waiter(waiters: &CacheWaiters, query_id: &str) {
    waiters.lock().remove(query_id);
}

/// Drop every waiter belonging to `job_id` so a query a finished or aborted job
/// left in flight can't leak its slot.
pub fn forget_cache_waiters_for_job(waiters: &CacheWaiters, job_id: &str) {
    waiters.lock().retain(|_, w| w.job_id != job_id);
}

/// A pending `QueryKnownDerivations`: its reply channel plus the owning
/// `job_id` so a finished or aborted job can drop any query it left in flight.
pub struct KnownDerivationWaiter {
    job_id: String,
    reply: oneshot::Sender<Vec<String>>,
}

/// Shared map from a unique per-query id to its pending `QueryKnownDerivations`.
/// Keying by the query id (not the `job_id`) lets one job keep several queries
/// in flight without their replies colliding; the waiter still carries its
/// `job_id` so job cleanup can drop the whole set.
pub type KnownDerivationWaiters = Arc<Mutex<HashMap<String, KnownDerivationWaiter>>>;

/// Register a oneshot for `query_id` (scoped to `job_id`) and hand back its
/// receiver.
pub fn register_known_derivation_waiter(
    waiters: &KnownDerivationWaiters,
    query_id: String,
    job_id: String,
) -> oneshot::Receiver<Vec<String>> {
    let (reply, rx) = oneshot::channel();
    waiters
        .lock()
        .insert(query_id, KnownDerivationWaiter { job_id, reply });
    rx
}

/// Deliver a `KnownDerivations` to the waiter that sent `query_id`. Returns
/// false (dropping the reply) when no waiter is registered - a late reply for
/// an already-timed-out or superseded query.
pub fn deliver_known_derivations(
    waiters: &KnownDerivationWaiters,
    query_id: &str,
    known: Vec<String>,
) -> bool {
    match waiters.lock().remove(query_id) {
        Some(w) => {
            let _ = w.reply.send(known);
            true
        }
        None => false,
    }
}

/// Drop every waiter belonging to `job_id` so a query a finished or aborted job
/// left in flight can't leak its slot.
pub fn forget_known_derivation_waiters_for_job(waiters: &KnownDerivationWaiters, job_id: &str) {
    waiters.lock().retain(|_, w| w.job_id != job_id);
}

/// The dispatch id a job reports under, shared between the job task and the
/// dispatch loop, which replaces it when the server hands the same job out
/// again while it is still running here.
#[derive(Clone, Debug)]
pub struct DispatchHandle(Arc<Mutex<String>>);

impl DispatchHandle {
    pub fn new(dispatch: String) -> Self {
        Self(Arc::new(Mutex::new(dispatch)))
    }

    pub fn get(&self) -> String {
        self.0.lock().clone()
    }

    pub fn set(&self, dispatch: String) {
        *self.0.lock() = dispatch;
    }
}

/// Query the server's known-derivation set in [`CACHE_QUERY_MAX_PATHS`]-sized
/// batches, [`CACHE_QUERY_WINDOW`] of them in flight, and concatenate the
/// answers in request order. See [`cache_query_with_timeout`] for why a whole
/// eval's set must never ride in a single message.
pub async fn known_derivations_with_timeout(
    job_id: &str,
    writer: &ProtoWriter,
    waiters: &KnownDerivationWaiters,
    drv_paths: Vec<String>,
) -> Result<Vec<String>> {
    let chunks = drv_paths
        .chunks(CACHE_QUERY_MAX_PATHS)
        .map(<[String]>::to_vec);
    let answers: Vec<Vec<String>> = futures::stream::iter(chunks)
        .map(|chunk| known_derivations_chunk(job_id, writer, waiters, chunk))
        .buffered(CACHE_QUERY_WINDOW)
        .try_collect()
        .await?;

    Ok(answers.into_iter().flatten().collect())
}

/// Send one `QueryKnownDerivations` and wait for the `KnownDerivations` that
/// echoes its `query_id`, with a hard timeout so a stalled dispatch loop can't
/// hang the eval task.
pub async fn known_derivations_chunk(
    job_id: &str,
    writer: &ProtoWriter,
    waiters: &KnownDerivationWaiters,
    drv_paths: Vec<String>,
) -> Result<Vec<String>> {
    let path_count = drv_paths.len();
    let query_id = uuid::Uuid::now_v7().to_string();
    let rx = register_known_derivation_waiter(waiters, query_id.clone(), job_id.to_owned());
    writer
        .send(ClientMessage::QueryKnownDerivations {
            job_id: job_id.to_owned(),
            query_id: query_id.clone(),
            drv_paths,
        })
        .await?;
    match tokio::time::timeout(CACHE_QUERY_TIMEOUT, rx).await {
        Ok(Ok(known)) => Ok(known),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "known-derivation waiter dropped - connection closed or superseded?"
        )),
        Err(_) => {
            waiters.lock().remove(&query_id);
            Err(anyhow::anyhow!(
                "QueryKnownDerivations for {} paths timed out after {}s (job_id={job_id}, query_id={query_id})",
                path_count,
                CACHE_QUERY_TIMEOUT.as_secs(),
            ))
        }
    }
}

/// Query cache state for `paths` in [`CACHE_QUERY_MAX_PATHS`]-sized batches,
/// [`CACHE_QUERY_WINDOW`] of them in flight, and concatenate the answers in
/// request order.
///
/// One chunk is bounded because an eval's full path set (tens of thousands)
/// would otherwise serialise into a multi-MB request whose `CacheStatus` reply
/// is larger still: with both peers mid-write the socket buffers fill in both
/// directions, neither dispatch loop gets back to reading, and the connection
/// wedges until the worker's send timeout tears it down. Several such chunks
/// in flight are safe because each is bounded on its own and every reply
/// carries the `query_id` of the query it answers, so completion order is free
/// while the concatenation stays in request order.
pub async fn cache_query_with_timeout(
    job_id: &str,
    writer: &ProtoWriter,
    cache_waiters: &CacheWaiters,
    paths: Vec<String>,
    nar_sizes: Vec<Option<u64>>,
    mode: QueryMode,
    external: bool,
) -> Result<Vec<CachedPath>> {
    // A Push carries one size per path or the server rejects it. A caller that
    // cannot know them yet says so per path rather than sending none.
    let nar_sizes = match mode {
        QueryMode::Push if nar_sizes.len() != paths.len() => vec![None; paths.len()],
        _ => nar_sizes,
    };
    let chunks: Vec<(Vec<String>, Vec<Option<u64>>)> = paths
        .chunks(CACHE_QUERY_MAX_PATHS)
        .enumerate()
        .map(|(i, chunk)| {
            let sizes = nar_sizes
                .iter()
                .skip(i * CACHE_QUERY_MAX_PATHS)
                .take(chunk.len())
                .copied()
                .collect();
            (chunk.to_vec(), sizes)
        })
        .collect();
    let answers: Vec<Vec<CachedPath>> = futures::stream::iter(chunks)
        .map(|(chunk, sizes)| {
            cache_query_chunk(job_id, writer, cache_waiters, chunk, sizes, mode, external)
        })
        .buffered(CACHE_QUERY_WINDOW)
        .try_collect()
        .await?;

    Ok(answers.into_iter().flatten().collect())
}

/// Send one `CacheQuery` and wait for the matching `CacheStatus`, with a hard
/// timeout so a stalled dispatch loop can't hang the eval task forever.
pub async fn cache_query_chunk(
    job_id: &str,
    writer: &ProtoWriter,
    cache_waiters: &CacheWaiters,
    paths: Vec<String>,
    nar_sizes: Vec<Option<u64>>,
    mode: QueryMode,
    external: bool,
) -> Result<Vec<CachedPath>> {
    let path_count = paths.len();
    let query_id = uuid::Uuid::now_v7().to_string();
    let rx = register_cache_waiter(cache_waiters, query_id.clone(), job_id.to_owned());
    writer
        .send(ClientMessage::CacheQuery {
            job_id: job_id.to_owned(),
            query_id: query_id.clone(),
            paths,
            mode,
            nar_sizes,
            external,
        })
        .await?;
    match tokio::time::timeout(CACHE_QUERY_TIMEOUT, rx).await {
        // Server could determine cache state: authoritative cached/uncached list.
        Ok(Ok(Ok(cached))) => Ok(cached),
        // Server-side `CacheError`: indeterminate, not "absent". Propagate as a
        // plain error so prefetch classifies it transient (retry) rather than a
        // terminal `InputsUnavailable`.
        Ok(Ok(Err(message))) => Err(anyhow::anyhow!("CacheQuery failed server-side: {message}")),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "cache waiter dropped - connection closed or superseded?"
        )),
        Err(_) => {
            // Drop the waiter so a late reply doesn't deliver to a closed
            // channel and log a spurious warning later.
            forget_cache_waiter(cache_waiters, &query_id);
            Err(anyhow::anyhow!(
                "CacheQuery for {} paths timed out after {}s waiting for reply (job_id={job_id}, query_id={query_id})",
                path_count,
                CACHE_QUERY_TIMEOUT.as_secs(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `CacheQuery` reply is correlated by its unique query id, never the
    /// `job_id`: two queries in flight for the same job must not steal each
    /// other's reply, a stale/unknown reply is dropped, and job cleanup frees a
    /// query left pending.
    #[test]
    fn cache_replies_correlate_by_query_id_not_job_id() {
        use tokio::sync::oneshot::error::TryRecvError;
        let waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut rx1 = register_cache_waiter(&waiters, "q1".to_string(), "job-A".to_string());
        let mut rx2 = register_cache_waiter(&waiters, "q2".to_string(), "job-A".to_string());

        assert!(deliver_cache_reply(&waiters, "q2", Ok(vec![])));
        assert!(matches!(rx2.try_recv(), Ok(Ok(_))));
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Empty));

        assert!(!deliver_cache_reply(&waiters, "gone", Ok(vec![])));

        forget_cache_waiters_for_job(&waiters, "job-A");
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Closed));
    }

    /// Job cleanup drops every known-derivation query that job left in flight
    /// and nothing belonging to another job.
    #[test]
    fn job_cleanup_drops_only_that_jobs_known_derivation_waiters() {
        use tokio::sync::oneshot::error::TryRecvError;
        let waiters: KnownDerivationWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut rx_a1 =
            register_known_derivation_waiter(&waiters, "q1".to_string(), "job-A".to_string());
        let mut rx_a2 =
            register_known_derivation_waiter(&waiters, "q2".to_string(), "job-A".to_string());
        let mut rx_b =
            register_known_derivation_waiter(&waiters, "q3".to_string(), "job-B".to_string());

        forget_known_derivation_waiters_for_job(&waiters, "job-A");

        assert_eq!(rx_a1.try_recv(), Err(TryRecvError::Closed));
        assert_eq!(rx_a2.try_recv(), Err(TryRecvError::Closed));
        assert_eq!(waiters.lock().len(), 1);
        assert!(deliver_known_derivations(
            &waiters,
            "q3",
            vec!["/nix/store/d.drv".to_string()]
        ));
        assert_eq!(rx_b.try_recv(), Ok(vec!["/nix/store/d.drv".to_string()]));
    }
}
