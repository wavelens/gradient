/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip, fetch_nar_stream};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;
use gradient_core::ServerState;
use gradient_storage::nar_extract::nar_reader_from_stream;
use harmonia_file_nar::parse_nar;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

type DirFrame = (Option<String>, BTreeMap<String, Box<FileTree>>);

#[derive(Debug, Serialize)]
pub struct NarList {
    version: u16,
    root: FileTree,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum FileTree {
    Regular {
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        executable: bool,
        size: u64,
        #[serde(rename = "narOffset")]
        nar_offset: Option<u64>,
    },
    Symlink {
        target: String,
    },
    Directory {
        entries: BTreeMap<String, Box<FileTree>>,
    },
}

pub async fn ls(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, hash)): Path<(String, String)>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    let _ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;
    let (_effective_hash, _size, stream) = fetch_nar_stream(&state, &hash).await?;
    let reader = nar_reader_from_stream(stream);

    let root = walk_nar(reader)
        .await
        .map_err(|e| WebError::internal(format!("NAR walk failed: {}", e)))?;

    Ok(Json(NarList { version: 1, root }).into_response())
}

async fn walk_nar<R>(reader: R) -> io::Result<FileTree>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use harmonia_file_nar::archive::NarEvent;

    let mut stream = parse_nar(reader);
    let mut stack: Vec<DirFrame> = Vec::new();
    let mut result: Option<FileTree> = None;

    while let Some(ev) = stream.next().await {
        let ev = ev?;
        match ev {
            NarEvent::File {
                name,
                executable,
                size,
                mut reader,
            } => {
                tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
                let node = FileTree::Regular {
                    executable,
                    size,
                    nar_offset: None,
                };
                insert_node(&mut stack, &mut result, bytes_to_str(&name)?, node);
            }
            NarEvent::Symlink { name, target } => {
                let node = FileTree::Symlink {
                    target: bytes_to_str(&target)?,
                };
                insert_node(&mut stack, &mut result, bytes_to_str(&name)?, node);
            }
            NarEvent::StartDirectory { name } => {
                let name_opt = if name.is_empty() {
                    None
                } else {
                    Some(bytes_to_str(&name)?)
                };
                stack.push((name_opt, BTreeMap::new()));
            }
            NarEvent::EndDirectory => {
                let (name, entries) = stack
                    .pop()
                    .ok_or_else(|| io::Error::other("EndDirectory without StartDirectory"))?;
                let node = FileTree::Directory { entries };
                if let Some(name) = name {
                    insert_node(&mut stack, &mut result, name, node);
                } else {
                    result = Some(node);
                }
            }
        }
    }

    result.ok_or_else(|| io::Error::other("NAR ended without root"))
}

fn insert_node(
    stack: &mut [DirFrame],
    result: &mut Option<FileTree>,
    name: String,
    node: FileTree,
) {
    if let Some((_, entries)) = stack.last_mut() {
        entries.insert(name, Box::new(node));
    } else {
        *result = Some(node);
    }
}

fn bytes_to_str(b: &bytes::Bytes) -> io::Result<String> {
    std::str::from_utf8(b)
        .map(|s| s.to_owned())
        .map_err(|e| io::Error::other(format!("non-UTF-8 entry name: {e}")))
}
