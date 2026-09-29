/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use anyhow::{Context, Result, bail};
use gradient_util::sync::Mutex;
use gradient_wire::messages::{ClientMessage, ServerMessage};
use gradient_wire::types::{GrantTarget, UploadMetadata, UploadObject, UploadOutcome};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit, oneshot};

use crate::connection::ProtoWriter;

pub const MAX_UPLOAD_ATTEMPTS: u32 = 3;

struct Waiter {
    job_id: String,
    grant: Option<oneshot::Sender<GrantTarget>>,
    outcome: Option<oneshot::Sender<UploadOutcome>>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    waiters: HashMap<u64, Waiter>,
    jobs: HashMap<String, Weak<Semaphore>>,
}

/// Every upload this worker sends: requested, granted, transferred, then
/// acknowledged by the server. A slot bounds the transfer, not the commit after
/// it, so the server's graph sees a burst of commits it can batch. One job holds
/// at most half of the slots, so an evaluation pushing its whole closure never
/// queues build outputs behind it.
#[derive(Clone)]
pub struct UploadClient {
    inner: Arc<Mutex<Inner>>,
    writer: ProtoWriter,
    slots: Arc<Semaphore>,
    per_job: usize,
}

impl UploadClient {
    pub fn new(writer: ProtoWriter, max_outstanding: usize) -> Self {
        Self {
            inner: Arc::default(),
            writer,
            slots: Arc::new(Semaphore::new(max_outstanding.max(1))),
            per_job: (max_outstanding / 2).max(1),
        }
    }

    pub fn writer(&self) -> &ProtoWriter {
        &self.writer
    }

    pub fn deliver(&self, msg: ServerMessage) {
        let mut inner = self.inner.lock();
        match msg {
            ServerMessage::UploadGrant { request_id, target } => {
                if let Some(tx) = inner
                    .waiters
                    .get_mut(&request_id)
                    .and_then(|w| w.grant.take())
                {
                    let _ = tx.send(target);
                }
            }
            ServerMessage::UploadCommitted {
                request_id,
                outcome,
            } => {
                if let Some(tx) = inner
                    .waiters
                    .get_mut(&request_id)
                    .and_then(|w| w.outcome.take())
                {
                    let _ = tx.send(outcome);
                }
            }
            _ => {}
        }
    }

    /// Take one of this worker's upload slots for `object`; drive the returned
    /// [`Upload`] with [`Upload::next_grant`] and [`Upload::settle`].
    pub async fn start(&self, job_id: &str, object: UploadObject, size: u64) -> Result<Upload<'_>> {
        Ok(Upload {
            client: self,
            slots: Some(self.acquire_slots(job_id).await?),
            job_id: job_id.to_owned(),
            object,
            size,
            attempts: 0,
            waiting: None,
            open: None,
        })
    }

    pub async fn cancel_job(&self, job_id: &str) {
        let ids: Vec<u64> = {
            let mut inner = self.inner.lock();
            let ids = inner
                .waiters
                .iter()
                .filter(|(_, w)| w.job_id == job_id)
                .map(|(id, _)| *id)
                .collect();
            inner.waiters.retain(|_, w| w.job_id != job_id);
            ids
        };
        for request_id in ids {
            let _ = self
                .writer
                .send(ClientMessage::UploadCancel { request_id })
                .await;
        }
    }

    pub fn forget_job(&self, job_id: &str) {
        self.inner.lock().waiters.retain(|_, w| w.job_id != job_id);
    }

    fn register(
        &self,
        job_id: &str,
    ) -> (
        u64,
        oneshot::Receiver<GrantTarget>,
        oneshot::Receiver<UploadOutcome>,
    ) {
        let (grant_tx, grant_rx) = oneshot::channel();
        let (outcome_tx, outcome_rx) = oneshot::channel();
        let mut inner = self.inner.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.waiters.insert(
            id,
            Waiter {
                job_id: job_id.to_owned(),
                grant: Some(grant_tx),
                outcome: Some(outcome_tx),
            },
        );
        (id, grant_rx, outcome_rx)
    }

    async fn acquire_slots(&self, job_id: &str) -> Result<Slots<'_>> {
        let job = self
            .job_slots(job_id)
            .acquire_owned()
            .await
            .context("upload slots closed")?;
        let worker = self.slots.acquire().await.context("upload slots closed")?;
        Ok(Slots {
            _job: job,
            _worker: worker,
        })
    }

    fn job_slots(&self, job_id: &str) -> Arc<Semaphore> {
        let mut inner = self.inner.lock();
        if let Some(slots) = inner.jobs.get(job_id).and_then(Weak::upgrade) {
            return slots;
        }
        inner.jobs.retain(|_, slots| slots.strong_count() > 0);
        let slots = Arc::new(Semaphore::new(self.per_job));
        inner.jobs.insert(job_id.to_owned(), Arc::downgrade(&slots));
        slots
    }

    fn forget(&self, request_id: u64) {
        self.inner.lock().waiters.remove(&request_id);
    }
}

struct Slots<'a> {
    _job: OwnedSemaphorePermit,
    _worker: SemaphorePermit<'a>,
}

/// One object's way through the handshake, holding a worker upload slot from
/// its request until its transfer is finished.
pub struct Upload<'a> {
    client: &'a UploadClient,
    slots: Option<Slots<'a>>,
    job_id: String,
    object: UploadObject,
    size: u64,
    attempts: u32,
    waiting: Option<u64>,
    open: Option<(u64, oneshot::Receiver<UploadOutcome>)>,
}

impl Upload<'_> {
    /// Request the object and wait for the server's grant; `None` means the
    /// server already has it and nothing is to be sent. The server may answer
    /// before granting (a rejection, or a retry when it cannot open a target).
    pub async fn next_grant(&mut self) -> Result<Option<(u64, GrantTarget)>> {
        if self.slots.is_none() {
            self.slots = Some(self.client.acquire_slots(&self.job_id).await?);
        }
        loop {
            if self.attempts == MAX_UPLOAD_ATTEMPTS {
                bail!(
                    "upload of {:?} still asked to retry after {MAX_UPLOAD_ATTEMPTS} attempts",
                    self.object
                );
            }
            self.attempts += 1;
            let (request_id, mut grant, mut outcome) = self.client.register(&self.job_id);
            self.waiting = Some(request_id);
            let sent = self
                .client
                .writer
                .send(ClientMessage::UploadRequest {
                    job_id: self.job_id.clone(),
                    request_id,
                    object: self.object.clone(),
                    size: self.size,
                })
                .await;
            if let Err(e) = sent {
                self.waiting = None;
                self.client.forget(request_id);
                return Err(e);
            }
            let answer = tokio::select! {
                biased;
                granted = &mut grant => Ok(granted),
                answered = &mut outcome => Err(answered),
            };
            self.waiting = None;
            match answer {
                Ok(granted) => {
                    return match granted.context("upload cancelled before its grant")? {
                        GrantTarget::Skip => {
                            self.client.forget(request_id);
                            Ok(None)
                        }
                        target => {
                            self.open = Some((request_id, outcome));
                            Ok(Some((request_id, target)))
                        }
                    };
                }
                Err(answered) => {
                    self.client.forget(request_id);
                    if self.stored(answered.context("upload cancelled before its grant")?)? {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// Report a finished transfer and wait for the commit: `true` once the
    /// object is stored, `false` when the server asks for another attempt. A
    /// failed transfer is cancelled so the server frees its permit at once.
    pub async fn settle(&mut self, transferred: Result<UploadMetadata>) -> Result<bool> {
        let (request_id, outcome) = self.open.take().context("no granted upload to settle")?;
        let finished = match transferred {
            Ok(metadata) => {
                self.client
                    .writer
                    .send(ClientMessage::UploadFinished {
                        request_id,
                        metadata,
                    })
                    .await
            }
            Err(e) => {
                let _ = self
                    .client
                    .writer
                    .send(ClientMessage::UploadCancel { request_id })
                    .await;
                Err(e)
            }
        };
        self.slots = None;
        let outcome = match finished {
            Ok(()) => outcome.await.context("upload cancelled before its commit"),
            Err(e) => Err(e),
        };
        self.client.forget(request_id);
        self.stored(outcome?)
    }

    /// `true` once the object is stored, `false` when the server asks for
    /// another attempt.
    fn stored(&self, outcome: UploadOutcome) -> Result<bool> {
        match outcome {
            UploadOutcome::Ok => Ok(true),
            UploadOutcome::Retry { reason } => {
                tracing::warn!(job_id = %self.job_id, object = ?self.object, attempt = self.attempts, %reason, "upload asked to retry");
                Ok(false)
            }
            UploadOutcome::Rejected { reason } => {
                bail!("upload of {:?} rejected: {reason}", self.object)
            }
        }
    }
}

impl Drop for Upload<'_> {
    fn drop(&mut self) {
        let abandoned = self
            .waiting
            .take()
            .or_else(|| self.open.take().map(|(id, _)| id));
        if let Some(request_id) = abandoned {
            self.client.forget(request_id);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let writer = self.client.writer.clone();
                runtime.spawn(async move {
                    let _ = writer
                        .send(ClientMessage::UploadCancel { request_id })
                        .await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_wire::testing::MockProtoServer;
    use std::time::Duration;

    async fn connected(
        max: usize,
    ) -> (
        UploadClient,
        gradient_wire::testing::MockServerConn,
        tokio::task::JoinHandle<()>,
    ) {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let accept = tokio::spawn(async move { server.accept().await });
        let conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, mut reader, _flush) = conn.split();
        let client = UploadClient::new(writer, max);
        let pump_client = client.clone();
        let pump = tokio::spawn(async move {
            while let Some(inbound) = reader.recv().await {
                if let gradient_wire::Inbound::Control(msg) = inbound {
                    pump_client.deliver(msg);
                }
            }
        });
        (client, accept.await.unwrap(), pump)
    }

    fn nar() -> UploadObject {
        UploadObject::Nar {
            store_path: format!("/nix/store/{}-p", "a".repeat(32)),
        }
    }

    fn meta() -> UploadMetadata {
        UploadMetadata::EvalCache { size_bytes: 1 }
    }

    async fn run(client: &UploadClient) -> Result<()> {
        let mut upload = client.start("build:1", nar(), 1).await?;
        while upload.next_grant().await?.is_some() {
            if upload.settle(Ok(meta())).await? {
                break;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn nothing_is_sent_before_the_grant() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!("request first")
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(200), server.recv())
                .await
                .is_err()
        );
        server
            .send(ServerMessage::UploadGrant {
                request_id,
                target: GrantTarget::Put { url: "u".into() },
            })
            .await
            .unwrap();
        assert!(matches!(
            server.recv().await.unwrap(),
            ClientMessage::UploadFinished { .. }
        ));
        server
            .send(ServerMessage::UploadCommitted {
                request_id,
                outcome: UploadOutcome::Ok,
            })
            .await
            .unwrap();
        upload.await.unwrap().unwrap();
    }

    /// A slot held through the commit capped an evaluation at one commit in
    /// flight per job slot, so the server's graph never saw a burst to batch.
    #[tokio::test]
    async fn an_upload_waiting_for_its_commit_frees_its_slot() {
        let (client, mut server, _pump) = connected(2).await;
        let first = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!("request first")
        };
        server
            .send(ServerMessage::UploadGrant {
                request_id,
                target: GrantTarget::Put { url: "u".into() },
            })
            .await
            .unwrap();
        assert!(matches!(
            server.recv().await.unwrap(),
            ClientMessage::UploadFinished { .. }
        ));

        let second = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let next = tokio::time::timeout(Duration::from_secs(5), server.recv())
            .await
            .expect("the second upload is requested while the first awaits its commit");
        assert!(matches!(next.unwrap(), ClientMessage::UploadRequest { .. }));

        server
            .send(ServerMessage::UploadCommitted {
                request_id,
                outcome: UploadOutcome::Ok,
            })
            .await
            .unwrap();
        first.await.unwrap().unwrap();
        second.abort();
    }

    #[tokio::test]
    async fn a_skip_settles_without_a_transfer() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move {
                let mut upload = client.start("build:1", nar(), 1).await?;
                assert!(upload.next_grant().await?.is_none(), "no transfer on skip");
                anyhow::Ok(())
            }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        server
            .send(ServerMessage::UploadGrant {
                request_id,
                target: GrantTarget::Skip,
            })
            .await
            .unwrap();
        upload.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn retries_request_again_and_give_up_after_the_limit() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        for _ in 0..MAX_UPLOAD_ATTEMPTS {
            let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap()
            else {
                panic!()
            };
            server
                .send(ServerMessage::UploadGrant {
                    request_id,
                    target: GrantTarget::Put { url: "u".into() },
                })
                .await
                .unwrap();
            let _finished = server.recv().await.unwrap();
            server
                .send(ServerMessage::UploadCommitted {
                    request_id,
                    outcome: UploadOutcome::Retry { reason: "x".into() },
                })
                .await
                .unwrap();
        }
        assert!(upload.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_rejected_upload_fails_at_once() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        server
            .send(ServerMessage::UploadGrant {
                request_id,
                target: GrantTarget::Put { url: "u".into() },
            })
            .await
            .unwrap();
        let _finished = server.recv().await.unwrap();
        server
            .send(ServerMessage::UploadCommitted {
                request_id,
                outcome: UploadOutcome::Rejected {
                    reason: "bad".into(),
                },
            })
            .await
            .unwrap();
        assert!(upload.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_rejection_before_the_grant_fails_the_upload() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        server
            .send(ServerMessage::UploadCommitted {
                request_id,
                outcome: UploadOutcome::Rejected {
                    reason: "job is not running".into(),
                },
            })
            .await
            .unwrap();
        let settled = tokio::time::timeout(Duration::from_secs(5), upload)
            .await
            .expect("the upload must not wait for a grant that never comes");
        assert!(settled.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_retry_before_the_grant_requests_again() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        server
            .send(ServerMessage::UploadCommitted {
                request_id,
                outcome: UploadOutcome::Retry {
                    reason: "no upload target".into(),
                },
            })
            .await
            .unwrap();
        let ClientMessage::UploadRequest {
            request_id: again, ..
        } = tokio::time::timeout(Duration::from_secs(5), server.recv())
            .await
            .expect("a retry asks again")
            .unwrap()
        else {
            panic!("a second request")
        };
        server
            .send(ServerMessage::UploadGrant {
                request_id: again,
                target: GrantTarget::Skip,
            })
            .await
            .unwrap();
        upload.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_upload_dropped_while_waiting_cancels_its_request() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        upload.abort();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server.recv())
                .await
                .expect("the server hears the request is gone")
                .unwrap(),
            ClientMessage::UploadCancel { request_id }
        );
    }

    #[tokio::test]
    async fn an_aborted_job_cancels_its_queued_uploads() {
        let (client, mut server, _pump) = connected(4).await;
        let upload = tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        client.cancel_job("build:1").await;
        assert_eq!(
            server.recv().await.unwrap(),
            ClientMessage::UploadCancel { request_id }
        );
        assert!(upload.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn one_job_leaves_slots_for_the_others() {
        let (client, mut server, _pump) = connected(2).await;
        for _ in 0..2 {
            let client = client.clone();
            tokio::spawn(async move {
                let _ = client.start("eval:1", nar(), 1).await?.next_grant().await;
                anyhow::Ok(())
            });
        }
        let ClientMessage::UploadRequest { job_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!(job_id, "eval:1");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), server.recv())
                .await
                .is_err(),
            "the eval waits on its own share"
        );
        tokio::spawn({
            let client = client.clone();
            async move { run(&client).await }
        });
        let ClientMessage::UploadRequest { job_id, .. } =
            tokio::time::timeout(Duration::from_secs(5), server.recv())
                .await
                .expect("a build output is not queued behind the eval")
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(job_id, "build:1");
    }

    #[tokio::test]
    async fn outstanding_requests_are_capped_per_worker() {
        let (client, mut server, _pump) = connected(1).await;
        for _ in 0..2 {
            let client = client.clone();
            tokio::spawn(async move { run(&client).await });
        }
        let ClientMessage::UploadRequest { request_id, .. } = server.recv().await.unwrap() else {
            panic!()
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(200), server.recv())
                .await
                .is_err(),
            "the second request waits for the slot"
        );
        server
            .send(ServerMessage::UploadGrant {
                request_id,
                target: GrantTarget::Skip,
            })
            .await
            .unwrap();
        assert!(matches!(
            server.recv().await.unwrap(),
            ClientMessage::UploadRequest { .. }
        ));
    }
}
