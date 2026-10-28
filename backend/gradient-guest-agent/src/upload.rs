/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use gradient_wire::guest::{AgentToWorker, GUEST_UPLOADS_SINCE};
use gradient_wire::types::{TREE_PATCH_OUTPUT, check_output_path};
use tokio::sync::{mpsc, oneshot};

const MAX_CHUNK_BYTES: usize = 1024 * 1024;

pub fn output_refusal(path: &str) -> Option<String> {
    if path == TREE_PATCH_OUTPUT {
        return Some(format!("{TREE_PATCH_OUTPUT} is written by Gradient"));
    }
    check_output_path(path).err().map(|e| e.to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadFailure {
    Lost,
    Refused(String),
    Body(String),
}

impl fmt::Display for UploadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadFailure::Lost => f.write_str("the worker connection was lost, retry the upload"),
            UploadFailure::Refused(reason) => f.write_str(reason),
            UploadFailure::Body(reason) => write!(f, "cannot read the upload: {reason}"),
        }
    }
}

#[derive(Clone)]
pub struct LinkedWorker {
    pub generation: u64,
    pub version: u16,
    pub outgoing: mpsc::Sender<AgentToWorker>,
}

#[derive(Default)]
pub struct WorkerLink {
    next_generation: AtomicU64,
    current: Mutex<Option<LinkedWorker>>,
}

impl WorkerLink {
    pub fn connect(&self, outgoing: mpsc::Sender<AgentToWorker>, version: u16) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        *self.lock() = Some(LinkedWorker {
            generation,
            version,
            outgoing,
        });
        generation
    }

    pub fn disconnect(&self, generation: u64) {
        let mut current = self.lock();
        if current.as_ref().map(|worker| worker.generation) == Some(generation) {
            *current = None;
        }
    }

    pub fn current(&self) -> Option<LinkedWorker> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<LinkedWorker>> {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct Waiting {
    generation: u64,
    settled: oneshot::Sender<Option<String>>,
}

#[derive(Default)]
pub struct UploadSender {
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, Waiting>>,
}

impl UploadSender {
    pub async fn upload<E: fmt::Display>(
        &self,
        link: &WorkerLink,
        path: &str,
        size: u64,
        body: impl Stream<Item = Result<Bytes, E>>,
    ) -> Result<(), UploadFailure> {
        let worker = link.current().ok_or(UploadFailure::Lost)?;
        if worker.version < GUEST_UPLOADS_SINCE {
            return Err(UploadFailure::Refused(
                "the worker does not accept outputs from this guest".to_owned(),
            ));
        }
        let upload_id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let settled = self.wait_for(upload_id, worker.generation);
        let _forget = Forget {
            sender: self,
            upload_id,
        };
        let start = AgentToWorker::UploadStart {
            upload_id,
            path: path.to_owned(),
            size,
        };
        send(&worker, start).await?;
        let streamed = stream_body(&worker, upload_id, body).await;
        send(&worker, AgentToWorker::UploadEnd { upload_id }).await?;
        streamed?;
        match settled.await {
            Ok(None) => Ok(()),
            Ok(Some(reason)) => Err(UploadFailure::Refused(reason)),
            Err(_) => Err(UploadFailure::Lost),
        }
    }

    pub async fn upload_bytes(
        &self,
        link: &WorkerLink,
        path: &str,
        data: Bytes,
    ) -> Result<(), UploadFailure> {
        let size = data.len() as u64;
        let body = futures::stream::once(async { Ok::<_, Infallible>(data) });
        self.upload(link, path, size, body).await
    }

    pub fn settled(&self, upload_id: u64, error: Option<String>) {
        if let Some(waiting) = self.lock().remove(&upload_id) {
            let _ = waiting.settled.send(error);
        }
    }

    pub fn connection_lost(&self, generation: u64) {
        self.lock()
            .retain(|_, waiting| waiting.generation != generation);
    }

    fn wait_for(&self, upload_id: u64, generation: u64) -> oneshot::Receiver<Option<String>> {
        let (settled, answer) = oneshot::channel();
        self.lock().insert(
            upload_id,
            Waiting {
                generation,
                settled,
            },
        );
        answer
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Waiting>> {
        self.waiting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct Forget<'a> {
    sender: &'a UploadSender,
    upload_id: u64,
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.sender.lock().remove(&self.upload_id);
    }
}

async fn stream_body<E: fmt::Display>(
    worker: &LinkedWorker,
    upload_id: u64,
    body: impl Stream<Item = Result<Bytes, E>>,
) -> Result<(), UploadFailure> {
    let mut body = std::pin::pin!(body);
    while let Some(read) = body.next().await {
        let data = read.map_err(|e| UploadFailure::Body(e.to_string()))?;
        for chunk in data.chunks(MAX_CHUNK_BYTES) {
            let chunk = AgentToWorker::UploadChunk {
                upload_id,
                data: chunk.to_vec(),
            };
            send(worker, chunk).await?;
        }
    }
    Ok(())
}

async fn send(worker: &LinkedWorker, message: AgentToWorker) -> Result<(), UploadFailure> {
    worker
        .outgoing
        .send(message)
        .await
        .map_err(|_| UploadFailure::Lost)
}

#[cfg(test)]
mod tests {
    use gradient_wire::guest::GUEST_PROTO_VERSIONS;

    use super::*;

    #[test]
    fn the_tree_patch_cannot_be_uploaded_by_the_program() {
        assert_eq!(
            output_refusal("gradient/tree.patch").as_deref(),
            Some("gradient/tree.patch is written by Gradient")
        );
        assert_eq!(output_refusal("gradient/continue.json"), None);
    }

    #[tokio::test]
    async fn a_lost_connection_fails_the_waiting_upload() {
        let link = WorkerLink::default();
        let (outgoing, mut sent) = mpsc::channel(8);
        let generation = link.connect(outgoing, *GUEST_PROTO_VERSIONS.end());
        let uploads = UploadSender::default();
        let body = futures::stream::iter([Ok::<_, Infallible>(Bytes::from_static(b"abc"))]);

        let worker = async {
            while let Some(message) = sent.recv().await {
                if matches!(message, AgentToWorker::UploadEnd { .. }) {
                    link.disconnect(generation);
                    uploads.connection_lost(generation);
                }
            }
        };
        let uploaded = uploads.upload(&link, "w.bin", 3, body);
        let result = tokio::select! {
            result = uploaded => result,
            () = worker => unreachable!("the worker side ends with the upload"),
        };

        assert_eq!(result, Err(UploadFailure::Lost));
        assert_eq!(
            result.unwrap_err().to_string(),
            "the worker connection was lost, retry the upload"
        );
    }
}
