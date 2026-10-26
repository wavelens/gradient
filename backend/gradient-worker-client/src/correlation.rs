/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::{StreamExt as _, TryStreamExt as _};
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    CACHE_QUERY_MAX_PATHS, CACHE_QUERY_TIMEOUT, CACHE_QUERY_WINDOW, CachedPath, ClientMessage,
    QueryMode,
};
use gradient_wire::traits::InputProgressSink;
use gradient_wire::types::{ImportOutcome, InputOutcome};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::RecvError;

use crate::connection::Unresponsive;
use crate::connection::reconnecting::ReconnectingWriter;

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

pub struct ImportWaiter {
    job_id: String,
    reply: oneshot::Sender<ImportOutcome>,
}

pub type ImportWaiters = Arc<Mutex<HashMap<String, ImportWaiter>>>;

pub fn register_import_waiter(
    waiters: &ImportWaiters,
    request_id: String,
    job_id: String,
) -> oneshot::Receiver<ImportOutcome> {
    let (reply, rx) = oneshot::channel();
    waiters
        .lock()
        .insert(request_id, ImportWaiter { job_id, reply });
    rx
}

pub fn deliver_import_result(
    waiters: &ImportWaiters,
    request_id: &str,
    outcome: ImportOutcome,
) -> bool {
    match waiters.lock().remove(request_id) {
        Some(w) => {
            let _ = w.reply.send(outcome);
            true
        }
        None => false,
    }
}

pub fn forget_import_waiters_for_job(waiters: &ImportWaiters, job_id: &str) {
    waiters.lock().retain(|_, w| w.job_id != job_id);
}

pub async fn request_across_sessions<T>(
    writer: &ReconnectingWriter,
    request: ClientMessage,
    answer: &mut oneshot::Receiver<T>,
    timeout: Option<Duration>,
) -> Result<Result<T, RecvError>> {
    loop {
        let session = writer.attached().await;
        if writer.send_now(request.clone()).await.is_err() {
            continue;
        }
        tokio::select! {
            answered = answer_within(&mut *answer, timeout) => return answered,
            () = writer.lost(session) => {}
        }
    }
}

async fn answer_within<T>(
    answer: &mut oneshot::Receiver<T>,
    timeout: Option<Duration>,
) -> Result<Result<T, RecvError>> {
    match timeout {
        Some(limit) => tokio::time::timeout(limit, answer)
            .await
            .map_err(|_| anyhow::Error::new(Unresponsive)),
        None => Ok(answer.await),
    }
}

pub async fn request_import(
    job_id: &str,
    writer: &ReconnectingWriter,
    waiters: &ImportWaiters,
    drv_paths: Vec<String>,
) -> Result<ImportOutcome> {
    let request_id = uuid::Uuid::now_v7().to_string();
    let mut rx = register_import_waiter(waiters, request_id.clone(), job_id.to_owned());
    let request = ClientMessage::ImportRequest {
        job_id: job_id.to_owned(),
        request_id,
        drv_paths,
    };
    request_across_sessions(writer, request, &mut rx, None)
        .await?
        .map_err(|_| anyhow::anyhow!("import waiter dropped - the job ended"))
}

pub struct InputWaiter {
    job_id: String,
    reply: oneshot::Sender<InputOutcome>,
    progress: Arc<dyn InputProgressSink>,
}

pub type InputWaiters = Arc<Mutex<HashMap<String, InputWaiter>>>;

pub fn register_input_waiter(
    waiters: &InputWaiters,
    request_id: String,
    job_id: String,
    progress: Arc<dyn InputProgressSink>,
) -> oneshot::Receiver<InputOutcome> {
    let (reply, rx) = oneshot::channel();
    waiters.lock().insert(
        request_id,
        InputWaiter {
            job_id,
            reply,
            progress,
        },
    );
    rx
}

pub fn deliver_input_result(
    waiters: &InputWaiters,
    request_id: &str,
    outcome: InputOutcome,
) -> bool {
    match waiters.lock().remove(request_id) {
        Some(w) => {
            let _ = w.reply.send(outcome);
            true
        }
        None => false,
    }
}

pub fn deliver_input_progress(
    waiters: &InputWaiters,
    request_id: &str,
    bytes_done: u64,
    bytes_total: Option<u64>,
) -> bool {
    let Some(progress) = waiters
        .lock()
        .get(request_id)
        .map(|w| Arc::clone(&w.progress))
    else {
        return false;
    };
    progress.transfer(bytes_done, bytes_total);
    true
}

pub fn forget_input_waiters_for_job(waiters: &InputWaiters, job_id: &str) {
    waiters.lock().retain(|_, w| w.job_id != job_id);
}

pub async fn request_input(
    job_id: &str,
    writer: &ReconnectingWriter,
    waiters: &InputWaiters,
    request: String,
    progress: Arc<dyn InputProgressSink>,
) -> Result<InputOutcome> {
    let request_id = uuid::Uuid::now_v7().to_string();
    let mut rx = register_input_waiter(waiters, request_id.clone(), job_id.to_owned(), progress);
    let request = ClientMessage::InputRequest {
        job_id: job_id.to_owned(),
        request_id,
        request,
    };
    request_across_sessions(writer, request, &mut rx, None)
        .await?
        .map_err(|_| anyhow::anyhow!("input waiter dropped - the job ended"))
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
    writer: &ReconnectingWriter,
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
    writer: &ReconnectingWriter,
    waiters: &KnownDerivationWaiters,
    drv_paths: Vec<String>,
) -> Result<Vec<String>> {
    let path_count = drv_paths.len();
    let query_id = uuid::Uuid::now_v7().to_string();
    let mut rx = register_known_derivation_waiter(waiters, query_id.clone(), job_id.to_owned());
    let request = ClientMessage::QueryKnownDerivations {
        job_id: job_id.to_owned(),
        query_id: query_id.clone(),
        drv_paths,
    };
    match request_across_sessions(writer, request, &mut rx, Some(CACHE_QUERY_TIMEOUT)).await {
        Ok(Ok(known)) => Ok(known),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "known-derivation waiter dropped - the job ended or the query was superseded"
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
    writer: &ReconnectingWriter,
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
    writer: &ReconnectingWriter,
    cache_waiters: &CacheWaiters,
    paths: Vec<String>,
    nar_sizes: Vec<Option<u64>>,
    mode: QueryMode,
    external: bool,
) -> Result<Vec<CachedPath>> {
    let path_count = paths.len();
    let query_id = uuid::Uuid::now_v7().to_string();
    let mut rx = register_cache_waiter(cache_waiters, query_id.clone(), job_id.to_owned());
    let request = ClientMessage::CacheQuery {
        job_id: job_id.to_owned(),
        query_id: query_id.clone(),
        paths,
        mode,
        nar_sizes,
        external,
    };
    match request_across_sessions(writer, request, &mut rx, Some(CACHE_QUERY_TIMEOUT)).await {
        Ok(Ok(Ok(cached))) => Ok(cached),
        // A server-side `CacheError` is indeterminate, not "absent". Prefetch must retry it as an
        // outage instead of a terminal `InputsUnavailable`.
        Ok(Ok(Err(message))) => Err(anyhow::Error::new(crate::connection::Unresponsive)
            .context(format!("CacheQuery failed server-side: {message}"))),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "cache waiter dropped - the job ended or the query was superseded"
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

    #[test]
    fn an_import_result_reaches_its_request_and_job_cleanup_drops_the_rest() {
        use tokio::sync::oneshot::error::TryRecvError;
        let waiters: ImportWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut rx_a = register_import_waiter(&waiters, "r1".to_string(), "job-A".to_string());
        let mut rx_b = register_import_waiter(&waiters, "r2".to_string(), "job-B".to_string());

        assert!(deliver_import_result(
            &waiters,
            "r2",
            ImportOutcome::Completed {
                realisations: Vec::new()
            }
        ));
        assert_eq!(
            rx_b.try_recv(),
            Ok(ImportOutcome::Completed {
                realisations: Vec::new()
            })
        );
        assert!(!deliver_import_result(
            &waiters,
            "r2",
            ImportOutcome::Completed {
                realisations: Vec::new()
            }
        ));

        forget_import_waiters_for_job(&waiters, "job-A");
        assert_eq!(rx_a.try_recv(), Err(TryRecvError::Closed));
        assert!(waiters.lock().is_empty());
    }

    #[derive(Default)]
    struct RecordedProgress(Mutex<Vec<(u64, Option<u64>)>>);

    impl InputProgressSink for RecordedProgress {
        fn transfer(&self, bytes_done: u64, bytes_total: Option<u64>) {
            self.0.lock().push((bytes_done, bytes_total));
        }
    }

    #[test]
    fn an_input_result_reaches_its_waiter() {
        use tokio::sync::oneshot::error::TryRecvError;
        let waiters: InputWaiters = Arc::new(Mutex::new(HashMap::new()));
        let none = Arc::new(RecordedProgress::default());
        let mut rx_a = register_input_waiter(&waiters, "r1".into(), "job-A".into(), none.clone());
        let mut rx_b = register_input_waiter(&waiters, "r2".into(), "job-B".into(), none);
        let fetched = InputOutcome::Fetched {
            response: r#"{"storePath":"/nix/store/aaaa-source"}"#.into(),
        };

        assert!(deliver_input_result(&waiters, "r2", fetched.clone()));
        assert_eq!(rx_b.try_recv(), Ok(fetched.clone()));
        assert!(!deliver_input_result(&waiters, "r2", fetched));

        forget_input_waiters_for_job(&waiters, "job-A");
        assert_eq!(rx_a.try_recv(), Err(TryRecvError::Closed));
        assert!(waiters.lock().is_empty());
    }

    #[test]
    fn input_progress_reaches_the_sink_of_its_request() {
        let waiters: InputWaiters = Arc::new(Mutex::new(HashMap::new()));
        let first = Arc::new(RecordedProgress::default());
        let second = Arc::new(RecordedProgress::default());
        let _rx1 = register_input_waiter(&waiters, "r1".into(), "job".into(), first.clone());
        let _rx2 = register_input_waiter(&waiters, "r2".into(), "job".into(), second.clone());

        assert!(deliver_input_progress(&waiters, "r2", 512, Some(1024)));
        assert!(!deliver_input_progress(&waiters, "gone", 1, None));

        assert!(first.0.lock().is_empty());
        assert_eq!(*second.0.lock(), vec![(512, Some(1024))]);
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test stands in for the server by hand"
    )]
    async fn a_query_cut_by_a_session_loss_is_asked_again_on_the_next_session() {
        let waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let writer = ReconnectingWriter::new();
        let mut first = crate::testing::mock_session().await;
        writer.attach(first.writer.clone()).await.unwrap();

        let query = tokio::spawn({
            let (writer, waiters) = (writer.clone(), Arc::clone(&waiters));
            async move {
                cache_query_chunk(
                    "build:1",
                    &writer,
                    &waiters,
                    vec!["/nix/store/a".into()],
                    vec![None],
                    QueryMode::Pull,
                    false,
                )
                .await
            }
        });
        let ClientMessage::CacheQuery { query_id, .. } = first.server.recv().await.unwrap() else {
            panic!("a cache query first");
        };

        writer.detach();
        let mut second = crate::testing::mock_session().await;
        writer.attach(second.writer.clone()).await.unwrap();
        let ClientMessage::CacheQuery {
            query_id: again, ..
        } = second.server.recv().await.unwrap()
        else {
            panic!("the query is asked again");
        };
        assert_eq!(again, query_id);

        assert!(deliver_cache_reply(&waiters, &query_id, Ok(vec![])));
        assert_eq!(query.await.unwrap().unwrap(), vec![]);
    }
}
