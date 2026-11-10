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

const MAX_OPEN_UPLOADS_PER_JOB: usize = 64;

pub fn guest_upload_staging_bytes_from_requested_disk_mb(disk_mb: u64) -> u64 {
    let disk_mb = match disk_mb {
        0 => gradient_wire::types::GuestSize::default_request().disk_mb,
        requested => requested,
    };
    disk_mb.saturating_mul(1024 * 1024)
}

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
    settling: JoinSet<(u64, u64, Option<String>)>,
    guest_upload_bytes_limit: u64,
    claimed_staged_upload_bytes: u64,
}

impl GuestUploads {
    pub fn new(dir: PathBuf, guest_upload_bytes_limit: u64) -> Self {
        Self {
            dir,
            open: HashMap::new(),
            settling: JoinSet::new(),
            guest_upload_bytes_limit,
            claimed_staged_upload_bytes: 0,
        }
    }

    pub async fn start(&mut self, upload_id: u64, path: String, size: u64) -> Result<()> {
        if self.open.len() >= MAX_OPEN_UPLOADS_PER_JOB {
            bail!("Upload {upload_id} rejected: {MAX_OPEN_UPLOADS_PER_JOB} uploads already open");
        }
        let free = self
            .guest_upload_bytes_limit
            .saturating_sub(self.claimed_staged_upload_bytes);
        if size > free {
            let reason = format!(
                "Upload {upload_id} rejected: {size} bytes exceed the {free} bytes left for uploads in this job"
            );
            self.open
                .insert(upload_id, StagedUpload::Failed(reason.clone()));
            bail!(reason);
        }
        let staged = self.dir.join(upload_id.to_string());
        let (upload, created) = match self.create(&staged).await {
            Ok(file) => {
                self.claimed_staged_upload_bytes += size;
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
        if let Some(StagedUpload::Writing { staged, size, .. }) =
            self.open.insert(upload_id, StagedUpload::Failed(reason))
        {
            remove_staged(&staged).await;
            self.claimed_staged_upload_bytes -= size;
        }
    }

    pub fn end(
        &mut self,
        upload_id: u64,
        upload: impl FnOnce(String, PathBuf, u64) -> OutputUpload,
    ) {
        let mut claimed = 0;
        let settling: BoxFuture<'static, Option<String>> = match self.open.remove(&upload_id) {
            Some(StagedUpload::Writing {
                path,
                file,
                staged,
                size,
                written,
            }) => {
                claimed = size;
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
            .spawn(async move { (upload_id, claimed, settling.await) });
    }

    pub async fn next_settled(&mut self) -> Option<(u64, Option<String>)> {
        loop {
            match self.settling.join_next().await? {
                Ok((upload_id, claimed, error)) => {
                    self.claimed_staged_upload_bytes -= claimed;
                    return Some((upload_id, error));
                }
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
        let mut uploads = GuestUploads::new(dir.path().to_owned(), 1024);
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
    async fn an_upload_beyond_the_job_byte_limit_is_refused_until_space_frees() {
        let dir = tempfile::tempdir().unwrap();
        let mut uploads = GuestUploads::new(dir.path().to_owned(), 5);
        uploads.start(1, "a.bin".into(), 3).await.unwrap();

        let error = uploads.start(2, "b.bin".into(), 3).await.unwrap_err();
        uploads.end(2, |_, _, _| Box::pin(async { Ok(()) }));
        assert_eq!(
            error.to_string(),
            "Upload 2 rejected: 3 bytes exceed the 2 bytes left for uploads in this job"
        );
        assert_eq!(
            uploads.next_settled().await,
            Some((2, Some(error.to_string())))
        );

        uploads.chunk(1, b"abc").await.unwrap();
        uploads.end(1, |_, _, _| Box::pin(async { Ok(()) }));
        assert_eq!(uploads.next_settled().await, Some((1, None)));
        uploads.start(3, "c.bin".into(), 5).await.unwrap();
    }

    #[tokio::test]
    async fn opening_more_uploads_than_the_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut uploads = GuestUploads::new(dir.path().to_owned(), 1024);
        for upload_id in 0..MAX_OPEN_UPLOADS_PER_JOB as u64 {
            uploads.start(upload_id, "f".into(), 1).await.unwrap();
        }

        let error = uploads.start(999, "f".into(), 1).await.unwrap_err();

        assert_eq!(
            error.to_string(),
            format!("Upload 999 rejected: {MAX_OPEN_UPLOADS_PER_JOB} uploads already open")
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
            let (mut last_chunk, mut finished) = (false, false);
            while !(last_chunk && finished) {
                match conn.recv().await.unwrap() {
                    ClientMessage::UploadChunk { data, is_final, .. } => {
                        received.extend_from_slice(&data);
                        last_chunk |= is_final;
                    }
                    ClientMessage::UploadFinished { .. } => finished = true,
                    other => panic!("unexpected {other:?}"),
                }
            }
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
        let mut uploads = GuestUploads::new(dir.path().join("outputs"), 1024);

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
