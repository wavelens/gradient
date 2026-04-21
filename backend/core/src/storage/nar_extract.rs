/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Streaming single-file extractor for zstd-compressed NARs in `nar_storage`.
//!
//! Buffers the full compressed NAR and the extracted file contents in memory.
//! Intended for small artefacts (hydra-build-products, manifests, per-file
//! downloads) — not for streaming large outputs.

use async_compression::tokio::bufread::ZstdDecoder;
use futures::StreamExt as _;
use harmonia_nar::{NarEvent, parse_nar};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, BufReader};

const MAX_PREALLOC: usize = 16 * 1024 * 1024; // 16 MiB

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("file not found in NAR")]
    NotFound,
    #[error("NAR io error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug)]
pub struct ExtractedFile {
    pub contents: Vec<u8>,
    pub executable: bool,
    pub size: u64,
}

pub async fn extract_file_from_nar_bytes(
    compressed: Vec<u8>,
    relative_path: &str,
) -> Result<ExtractedFile, ExtractError> {
    let reader = BufReader::new(io::Cursor::new(compressed));
    let decoder = ZstdDecoder::new(reader);
    extract_file_from_reader(decoder, relative_path).await
}

pub async fn extract_file_from_reader<R>(
    reader: R,
    relative_path: &str,
) -> Result<ExtractedFile, ExtractError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let target: Vec<&str> = relative_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    let mut stream = parse_nar(reader);
    let mut stack: Vec<Vec<u8>> = Vec::new();

    while let Some(ev) = stream.next().await {
        let ev = ev?;
        match ev {
            NarEvent::StartDirectory { name } => {
                if !name.is_empty() {
                    stack.push(name.to_vec());
                }
            }
            NarEvent::EndDirectory => {
                stack.pop();
            }
            NarEvent::Symlink { .. } => {}
            NarEvent::File {
                name,
                executable,
                size,
                mut reader,
            } => {
                if path_matches(&stack, name.as_ref(), &target) {
                    let cap = std::cmp::min(size, MAX_PREALLOC as u64) as usize;
                    let mut buf = Vec::with_capacity(cap);
                    reader.read_to_end(&mut buf).await?;
                    return Ok(ExtractedFile {
                        contents: buf,
                        executable,
                        size,
                    });
                }
                tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
            }
        }
    }
    Err(ExtractError::NotFound)
}

fn path_matches(stack: &[Vec<u8>], name: &[u8], target: &[&str]) -> bool {
    if stack.len() + 1 != target.len() {
        return false;
    }
    for (i, component) in stack.iter().enumerate() {
        if component.as_slice() != target[i].as_bytes() {
            return false;
        }
    }
    name == target[target.len() - 1].as_bytes()
}
