/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! NAR transfer — send built store paths to the server.
//!
//! Two modes depending on server configuration:
//! - **Direct**: chunked [`ClientMessage::NarPush`] frames over the WebSocket
//!   (zstd-compressed, 64 KiB chunks).
//! - **S3**: server sends a [`ServerMessage::PresignedUpload`]; worker uploads
//!   directly to S3 then confirms with [`ClientMessage::NarReady`].
//!
//! TODO(1.4): implement both transfer modes.

use anyhow::Result;
use proto::messages::ClientMessage;
use tracing::debug;

use crate::connection::ProtoConnection;

/// Chunk size for direct NAR streaming (64 KiB).
const NAR_CHUNK_SIZE: usize = 64 * 1024;

/// Handles NAR upload for a single store path.
pub struct NarTransfer<'a> {
    job_id: String,
    conn: &'a mut ProtoConnection,
}

impl<'a> NarTransfer<'a> {
    pub fn new(job_id: String, conn: &'a mut ProtoConnection) -> Self {
        Self { job_id, conn }
    }

    /// Push a zstd-compressed NAR directly over the WebSocket in chunks.
    ///
    /// TODO(1.4): dump store path to NAR, compress with zstd, stream chunks.
    pub async fn push_direct(&mut self, store_path: &str, _data: Vec<u8>) -> Result<()> {
        debug!(store_path, "NAR push (direct) — stub");
        // Placeholder: send an empty final chunk.
        self.conn
            .send(ClientMessage::NarPush {
                job_id: self.job_id.clone(),
                store_path: store_path.to_owned(),
                data: vec![],
                offset: 0,
                is_final: true,
            })
            .await
    }

    /// Report that a NAR was uploaded directly to S3.
    ///
    /// TODO(1.4): upload to presigned URL, compute hash/size.
    pub async fn report_nar_ready(
        &mut self,
        store_path: &str,
        nar_size: u64,
        nar_hash: String,
    ) -> Result<()> {
        self.conn
            .send(ClientMessage::NarReady {
                job_id: self.job_id.clone(),
                store_path: store_path.to_owned(),
                nar_size,
                nar_hash,
            })
            .await
    }
}
