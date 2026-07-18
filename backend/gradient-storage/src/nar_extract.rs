/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Single-path extractor for zstd-compressed NARs in `nar_storage`.
//!
//! Buffers the full compressed NAR in memory. When the requested path is a
//! file inside the NAR, returns the file's bytes. When it is a directory,
//! collects the subtree into a tar archive and zstd-compresses it at a low
//! level (cheap CPU; large empty regions still compact well).
//!
//! Intended for build artefacts (hydra-build-products, manifests, per-path
//! downloads) where the producer chose a small entry - not for streaming
//! arbitrarily large outputs.

use async_compression::tokio::bufread::ZstdDecoder;
use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gradient_types::constants::{NAR_EXTRACT_MAX_PREALLOC, TAR_ZSTD_LEVEL};
use harmonia_file_nar::{NarEvent, parse_nar};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, BufReader};

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("path not found in NAR")]
    NotFound,
    #[error("NAR io error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug)]
pub enum Extracted {
    /// The path resolved to a regular file. `contents` is the decoded body.
    File {
        contents: Vec<u8>,
        executable: bool,
        size: u64,
    },
    /// The path resolved to a directory. `tar_zst` is a `tar.zst` archive of
    /// the subtree, with paths rooted at the matched directory's basename
    /// (so extracting recreates `<basename>/...`).
    Directory { tar_zst: Vec<u8> },
}

pub async fn extract_path_from_nar_bytes(
    compressed: Vec<u8>,
    relative_path: &str,
) -> Result<Extracted, ExtractError> {
    let reader = BufReader::new(io::Cursor::new(compressed));
    let decoder = ZstdDecoder::new(reader);
    extract_path_from_reader(decoder, relative_path).await
}

/// Wraps a compressed `.nar.zst` byte stream (as returned by
/// `NarStore::get_stream`) into an `AsyncRead` over the *decompressed* NAR, so a
/// caller can extract or enumerate a single path without ever buffering the
/// whole compressed object in memory.
pub fn nar_reader_from_stream(
    stream: BoxStream<'static, anyhow::Result<Bytes>>,
) -> impl tokio::io::AsyncRead + Unpin {
    let byte_stream = stream.map(|chunk| chunk.map_err(io::Error::other));
    ZstdDecoder::new(tokio_util::io::StreamReader::new(byte_stream))
}

pub async fn extract_path_from_reader<R>(
    reader: R,
    relative_path: &str,
) -> Result<Extracted, ExtractError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let target: Vec<&str> = relative_path.split('/').filter(|s| !s.is_empty()).collect();
    if target.is_empty() {
        return Err(ExtractError::NotFound);
    }
    let basename = target[target.len() - 1].to_owned();

    let mut stream = parse_nar(reader);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    // Tar collector: started when we enter a directory matching the target.
    // The recorded depth is `stack.len()` *immediately after* pushing the
    // matched directory; we finish when EndDirectory pops back below it.
    let mut collector: Option<Collector> = None;

    while let Some(ev) = stream.next().await {
        let ev = ev?;
        match ev {
            NarEvent::StartDirectory { name } => {
                let pushed = !name.is_empty();
                if pushed {
                    stack.push(name.to_vec());
                }
                if let Some(c) = collector.as_mut() {
                    // Inside a matched subtree: emit a directory entry.
                    let path = c.entry_path(&stack, None);
                    if !path.is_empty() {
                        c.append_dir(&path)?;
                    }
                } else if path_matches_stack(&stack, &target) {
                    let mut c = Collector::new(basename.clone(), stack.len());
                    // Emit the root directory itself.
                    c.append_dir(&basename)?;
                    collector = Some(c);
                }
            }
            NarEvent::EndDirectory => {
                let leaving = collector.as_ref().is_some_and(|c| stack.len() == c.depth);
                stack.pop();
                if leaving {
                    let c = collector.take().unwrap();
                    return Ok(Extracted::Directory {
                        tar_zst: c.finish()?,
                    });
                }
            }
            NarEvent::Symlink { name, target: link } => {
                if let Some(c) = collector.as_mut() {
                    let path = c.entry_path(&stack, Some(name.as_ref()));
                    c.append_symlink(&path, &link)?;
                }
            }
            NarEvent::File {
                name,
                executable,
                size,
                mut reader,
            } => {
                let in_collector = collector.is_some();
                let matches_file =
                    !in_collector && path_matches_file(&stack, name.as_ref(), &target);
                if matches_file {
                    let cap = std::cmp::min(size, NAR_EXTRACT_MAX_PREALLOC as u64) as usize;
                    let mut buf = Vec::with_capacity(cap);
                    reader.read_to_end(&mut buf).await?;
                    return Ok(Extracted::File {
                        contents: buf,
                        executable,
                        size,
                    });
                }
                if in_collector {
                    let cap = std::cmp::min(size, NAR_EXTRACT_MAX_PREALLOC as u64) as usize;
                    let mut buf = Vec::with_capacity(cap);
                    reader.read_to_end(&mut buf).await?;
                    let c = collector.as_mut().unwrap();
                    let path = c.entry_path(&stack, Some(name.as_ref()));
                    c.append_file(&path, &buf, executable)?;
                } else {
                    tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
                }
            }
        }
    }

    if let Some(c) = collector {
        // Reached EOF while still in the collector - happens when the matched
        // directory is the NAR root and its closing EndDirectory is the final
        // event before stream end (parser emits None after EndDirectory at
        // level 0).
        return Ok(Extracted::Directory {
            tar_zst: c.finish()?,
        });
    }
    Err(ExtractError::NotFound)
}

// ── Internals ────────────────────────────────────────────────────────────────

struct Collector {
    basename: String,
    /// `stack.len()` at the moment we entered the matched directory. We
    /// finish when EndDirectory has fired with `stack.len() == depth`
    /// (i.e. we are about to pop the matched directory itself).
    depth: usize,
    builder: tar::Builder<Vec<u8>>,
}

impl Collector {
    fn new(basename: String, depth: usize) -> Self {
        let mut builder = tar::Builder::new(Vec::new());
        builder.mode(tar::HeaderMode::Deterministic);
        Self {
            basename,
            depth,
            builder,
        }
    }

    /// Build a tar entry path: `<basename>/<components-below-match>[/<name>]`.
    /// Components below match are `stack[depth..]` (the entries inside the
    /// matched directory), joined with `/`. `name` is appended for File and
    /// Symlink events; pass `None` for StartDirectory.
    fn entry_path(&self, stack: &[Vec<u8>], name: Option<&[u8]>) -> String {
        let mut parts: Vec<String> = vec![self.basename.clone()];
        for component in stack.iter().skip(self.depth) {
            parts.push(String::from_utf8_lossy(component).into_owned());
        }
        if let Some(n) = name {
            parts.push(String::from_utf8_lossy(n).into_owned());
        }
        parts.join("/")
    }

    fn append_dir(&mut self, path: &str) -> io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        let dir_path = format!("{path}/");
        self.builder
            .append_data(&mut header, &dir_path, std::io::empty())
    }

    fn append_file(&mut self, path: &str, data: &[u8], executable: bool) -> io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(if executable { 0o755 } else { 0o644 });
        header.set_size(data.len() as u64);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        self.builder.append_data(&mut header, path, data)
    }

    fn append_symlink(&mut self, path: &str, link_target: &[u8]) -> io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        let target_str = String::from_utf8_lossy(link_target);
        self.builder
            .append_link(&mut header, path, target_str.as_ref())
    }

    fn finish(self) -> io::Result<Vec<u8>> {
        let tar_bytes = self.builder.into_inner()?;
        // zstd::encode_all is sync; we're already fully buffered, so this is
        // a normal in-memory transform - no async wrapper buys us anything.
        zstd::encode_all(std::io::Cursor::new(tar_bytes), TAR_ZSTD_LEVEL)
    }
}

fn path_matches_file(stack: &[Vec<u8>], name: &[u8], target: &[&str]) -> bool {
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

/// Stack-vs-target match for the moment a directory is entered: stack has
/// just been pushed with the directory's name, so its length must equal the
/// target's length and every component must match.
fn path_matches_stack(stack: &[Vec<u8>], target: &[&str]) -> bool {
    if stack.len() != target.len() {
        return false;
    }
    for (i, component) in stack.iter().enumerate() {
        if component.as_slice() != target[i].as_bytes() {
            return false;
        }
    }
    true
}
