/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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

pub struct CacheWaiter {
    job_id: String,
    reply: oneshot::Sender<Result<Vec<CachedPath>, String>>,
}

pub type CacheWaiters = Arc<Mutex<HashMap<String, CacheWaiter>>>;

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

pub fn forget_cache_waiter(waiters: &CacheWaiters, query_id: &str) {
    waiters.lock().remove(query_id);
}

pub fn forget_cache_waiters_for_job(waiters: &CacheWaiters, job_id: &str) {
    waiters.lock().retain(|_, w| w.job_id != job_id);
}

pub struct KnownDerivationWaiter {
    job_id: String,
    reply: oneshot::Sender<Vec<String>>,
}

pub type KnownDerivationWaiters = Arc<Mutex<HashMap<String, KnownDerivationWaiter>>>;

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

pub fn forget_known_derivation_waiters_for_job(waiters: &KnownDerivationWaiters, job_id: &str) {
    waiters.lock().retain(|_, w| w.job_id != job_id);
}

#[derive(Clone, Debug)]
pub struct AssignmentHandle(Arc<Mutex<String>>);

impl AssignmentHandle {
    pub fn new(assignment_id: String) -> Self {
        Self(Arc::new(Mutex::new(assignment_id)))
    }

    pub fn get(&self) -> String {
        self.0.lock().clone()
    }

    pub fn set(&self, assignment_id: String) {
        *self.0.lock() = assignment_id;
    }
}

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

/// An eval's full path set would serialise into a multi-MB request with a larger reply. Both peers
/// would block mid-write with full socket buffers until the send timeout tore the connection down.
/// Each chunk is bounded, and replies are matched by `query_id`.
pub async fn cache_query_with_timeout(
    job_id: &str,
    writer: &ProtoWriter,
    cache_waiters: &CacheWaiters,
    paths: Vec<String>,
    nar_sizes: Vec<Option<u64>>,
    mode: QueryMode,
    external: bool,
) -> Result<Vec<CachedPath>> {
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
        Ok(Ok(Ok(cached))) => Ok(cached),
        // A server-side `CacheError` is indeterminate, not "absent". Prefetch must retry it as an
        // outage instead of a terminal `InputsUnavailable`.
        Ok(Ok(Err(message))) => Err(anyhow::Error::new(crate::connection::Unresponsive)
            .context(format!("CacheQuery failed server-side: {message}"))),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "cache waiter dropped - connection closed or superseded?"
        )),
        Err(_) => {
            forget_cache_waiter(cache_waiters, &query_id);
            Err(anyhow::Error::new(crate::connection::Unresponsive).context(format!(
                "CacheQuery for {} paths timed out after {}s waiting for reply (job_id={job_id}, query_id={query_id})",
                path_count,
                CACHE_QUERY_TIMEOUT.as_secs(),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
