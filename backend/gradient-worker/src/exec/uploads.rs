/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use futures::future::BoxFuture;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use tracing::debug;

pub type OutputUpload = BoxFuture<'static, Result<()>>;

enum StagedUpload {
    Writing {
        path: String,
        file: tokio::fs::File,
        staged: PathBuf,
        size: u64,
        written: u64,
    },
    Failed(String),
}

pub struct GuestUploads {
    dir: PathBuf,
    open: HashMap<u64, StagedUpload>,
    settling: JoinSet<(u64, Option<String>)>,
}

impl GuestUploads {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            open: HashMap::new(),
            settling: JoinSet::new(),
        }
    }

    pub async fn start(&mut self, upload_id: u64, path: String, size: u64) -> Result<()> {
        let staged = self.dir.join(upload_id.to_string());
        let (upload, created) = match self.create(&staged).await {
            Ok(file) => {
                let writing = StagedUpload::Writing {
                    path,
                    file,
                    staged,
                    size,
                    written: 0,
                };
                (writing, Ok(()))
            }
            Err(e) => (StagedUpload::Failed(format!("{e:#}")), Err(e)),
        };
        self.open.insert(upload_id, upload);
        created
    }

    async fn create(&self, staged: &PathBuf) -> Result<tokio::fs::File> {
        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| format!("create {}", self.dir.display()))?;
        tokio::fs::File::create(staged)
            .await
            .with_context(|| format!("create {}", staged.display()))
    }

    pub async fn chunk(&mut self, upload_id: u64, data: &[u8]) -> Result<()> {
        let Some(upload) = self.open.get_mut(&upload_id) else {
            bail!("upload {upload_id} is not open");
        };
        let written = write_chunk(upload_id, upload, data).await;
        if let Err(e) = &written {
            self.fail(upload_id, format!("{e:#}")).await;
        }
        written
    }

    async fn fail(&mut self, upload_id: u64, reason: String) {
        if let Some(StagedUpload::Writing { staged, .. }) =
            self.open.insert(upload_id, StagedUpload::Failed(reason))
        {
            remove_staged(&staged).await;
        }
    }

    pub fn end(
        &mut self,
        upload_id: u64,
        upload: impl FnOnce(String, PathBuf, u64) -> OutputUpload,
    ) {
        let settling: BoxFuture<'static, Option<String>> = match self.open.remove(&upload_id) {
            Some(StagedUpload::Writing {
                path,
                file,
                staged,
                size,
                written,
            }) => {
                let upload = upload(path, staged.clone(), size);
                Box::pin(async move {
                    let settled = settle(file, upload, size, written).await;
                    remove_staged(&staged).await;
                    settled.err().map(|e| format!("{e:#}"))
                })
            }
            Some(StagedUpload::Failed(reason)) => ready_answer(Some(reason)),
            None => ready_answer(Some(format!("upload {upload_id} is not open"))),
        };
        self.settling
            .spawn(async move { (upload_id, settling.await) });
    }

    pub async fn next_settled(&mut self) -> Option<(u64, Option<String>)> {
        loop {
            match self.settling.join_next().await? {
                Ok(settled) => return Some(settled),
                Err(e) => debug!(error = %e, "guest upload task ended without an answer"),
            }
        }
    }

    pub async fn drain(&mut self) -> Vec<(u64, Option<String>)> {
        let mut settled = Vec::new();
        while let Some(answer) = self.next_settled().await {
            settled.push(answer);
        }
        settled
    }
}

fn ready_answer(error: Option<String>) -> BoxFuture<'static, Option<String>> {
    Box::pin(std::future::ready(error))
}

async fn write_chunk(upload_id: u64, upload: &mut StagedUpload, data: &[u8]) -> Result<()> {
    let StagedUpload::Writing {
        file,
        size,
        written,
        ..
    } = upload
    else {
        return Ok(());
    };
    let total = *written + data.len() as u64;
    if total > *size {
        bail!("upload {upload_id} exceeds its {size} bytes");
    }
    file.write_all(data)
        .await
        .context("write the staged upload")?;
    *written = total;
    Ok(())
}

async fn settle(
    mut file: tokio::fs::File,
    upload: OutputUpload,
    size: u64,
    written: u64,
) -> Result<()> {
    if written != size {
        bail!("the upload ended after {written} of its {size} bytes");
    }
    file.flush().await.context("flush the staged upload")?;
    drop(file);
    upload.await
}

async fn remove_staged(staged: &PathBuf) {
    if let Err(e) = tokio::fs::remove_file(staged).await {
        debug!(path = %staged.display(), error = %e, "staged upload not removed");
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use gradient_test_support::prelude::MockProtoServer;
    use gradient_wire::messages::{ClientMessage, ServerMessage};
    use gradient_wire::types::{GrantTarget, UploadObject, UploadOutcome};
    use gradient_worker_client::connection::reconnecting::ReconnectingWriter;
    use gradient_worker_client::upload::UploadClient;

    use crate::proto::job::push_exec_output;

    use super::*;

    #[tokio::test]
    async fn chunks_beyond_the_announced_size_fail_the_upload() {
        let dir = tempfile::tempdir().unwrap();
        let mut uploads = GuestUploads::new(dir.path().to_owned());
        uploads.start(1, "w.bin".into(), 3).await.unwrap();

        let error = uploads.chunk(1, b"abcd").await.unwrap_err();
        uploads.end(1, |_, _, _| Box::pin(async { Ok(()) }));

        assert_eq!(error.to_string(), "upload 1 exceeds its 3 bytes");
        assert_eq!(
            uploads.next_settled().await,
            Some((1, Some("upload 1 exceeds its 3 bytes".into())))
        );
    }

    #[tokio::test]
    async fn an_ended_upload_is_settled_with_the_server_answer() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let fake_server = tokio::spawn(async move {
            let mut conn = server.accept().await;
            let ClientMessage::UploadRequest {
                request_id,
                object,
                size,
                ..
            } = conn.recv().await.unwrap()
            else {
                panic!("an upload request first");
            };
            conn.send(ServerMessage::UploadGrant {
                request_id,
                target: GrantTarget::Passthrough { resume_offset: 0 },
            })
            .await
            .unwrap();
            let mut received = Vec::new();
            loop {
                match conn.recv().await.unwrap() {
                    ClientMessage::UploadChunk { data, is_final, .. } => {
                        received.extend_from_slice(&data);
                        if is_final {
                            break;
                        }
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            let finished = conn.recv().await.unwrap();
            assert!(matches!(finished, ClientMessage::UploadFinished { .. }));
            conn.send(ServerMessage::UploadCommitted {
                request_id,
                outcome: UploadOutcome::Ok,
            })
            .await
            .unwrap();
            (object, size, received, conn)
        });
        let connection = gradient_worker_client::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, mut reader, _flush) = connection.split();
        let client = UploadClient::new(ReconnectingWriter::attached_to(writer), 4);
        let delivering = client.clone();
        tokio::spawn(async move {
            while let Some(message) = reader.recv().await {
                delivering.deliver(message);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let mut uploads = GuestUploads::new(dir.path().join("outputs"));

        uploads.start(1, "model/w.bin".into(), 3).await.unwrap();
        uploads.chunk(1, b"abc").await.unwrap();
        uploads.end(1, move |path, file, size| {
            Box::pin(async move {
                let object = UploadObject::ExecOutput {
                    job: "j".into(),
                    path,
                };
                push_exec_output(&client, "exec:j", object, &file, size).await
            })
        });

        assert_eq!(uploads.next_settled().await, Some((1, None)));
        let (object, size, received, _conn) = fake_server.await.unwrap();
        assert_eq!(
            object,
            UploadObject::ExecOutput {
                job: "j".into(),
                path: "model/w.bin".into()
            }
        );
        assert_eq!((size, received.as_slice()), (3, b"abc".as_slice()));
        assert!(!dir.path().join("outputs/1").exists());
    }
}
