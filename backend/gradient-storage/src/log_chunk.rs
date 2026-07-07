/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Splits a completed build log into line-bounded chunks for compression and
//! lazy serving. Each chunk records the SGR state active at its first byte so
//! it can be rendered standalone (see [`super::sgr::SgrState`]).

use super::sgr::SgrState;
use crate::log::LogStorage;
use anyhow::Result;
use gradient_types::ids::BuildAttemptId;

/// One uncompressed chunk plus the metadata persisted in `build_log_chunk`.
pub struct LogChunkDesc {
    pub text: String,
    pub byte_start: u64,
    pub line_start: u64,
    pub line_count: u32,
    pub color_prefix: String,
}

/// Split `log` into chunks on line boundaries, each ≈ `target_bytes` uncompressed.
/// A single line longer than `target_bytes` stays whole in its own chunk.
/// Each chunk records the SGR state active at its first byte (`color_prefix`).
pub fn chunk_log(log: &str, target_bytes: usize) -> Vec<LogChunkDesc> {
    let target = target_bytes.max(1);
    let mut chunks: Vec<LogChunkDesc> = Vec::new();
    let mut state = SgrState::default();

    let mut cur = String::new();
    let mut cur_lines: u32 = 0;
    let mut byte_start: u64 = 0;
    let mut line_start: u64 = 0;
    let mut prefix = state.to_prefix();

    for line in log.split_inclusive('\n') {
        if !cur.is_empty() && cur.len() + line.len() > target {
            state.apply_text(&cur);
            let len = cur.len() as u64;
            chunks.push(LogChunkDesc {
                text: std::mem::take(&mut cur),
                byte_start,
                line_start,
                line_count: cur_lines,
                color_prefix: std::mem::take(&mut prefix),
            });
            byte_start += len;
            line_start += cur_lines as u64;
            cur_lines = 0;
            prefix = state.to_prefix();
        }
        cur.push_str(line);
        cur_lines += 1;
    }

    if !cur.is_empty() {
        chunks.push(LogChunkDesc {
            text: cur,
            byte_start,
            line_start,
            line_count: cur_lines,
            color_prefix: prefix,
        });
    }
    chunks
}

/// A persisted chunk descriptor (mirrors a `build_log_chunk` row).
pub struct StoredChunkDesc {
    pub text: String,
    pub byte_start: u64,
    pub byte_len: u32,
    pub line_start: u64,
    pub line_count: u32,
    pub compressed_size: u32,
    pub color_prefix: String,
}

/// Split, zstd-compress, and write each chunk; return descriptors for the DB index.
/// Existing chunks for `log_key` are removed first so re-finalize is idempotent.
pub async fn compress_and_store_chunks(
    storage: &dyn LogStorage,
    log_key: BuildAttemptId,
    log: &str,
    target_bytes: usize,
) -> Result<Vec<StoredChunkDesc>> {
    storage.delete_chunks(log_key).await.ok();
    let mut out = Vec::new();
    for (index, c) in chunk_log(log, target_bytes).into_iter().enumerate() {
        let compressed = zstd::stream::encode_all(c.text.as_bytes(), 0)?;
        storage
            .write_chunk(log_key, index as u32, &compressed)
            .await?;
        out.push(StoredChunkDesc {
            byte_len: c.text.len() as u32,
            compressed_size: compressed.len() as u32,
            text: c.text,
            byte_start: c.byte_start,
            line_start: c.line_start,
            line_count: c.line_count,
            color_prefix: c.color_prefix,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::chunk_log;

    #[test]
    fn splits_on_line_boundary_respecting_target() {
        let log = "a\nb\nc\nd\n";
        let chunks = chunk_log(log, 4);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "a\nb\n");
        assert_eq!(chunks[0].line_start, 0);
        assert_eq!(chunks[0].line_count, 2);
        assert_eq!(chunks[1].text, "c\nd\n");
        assert_eq!(chunks[1].line_start, 2);
        assert_eq!(chunks[1].byte_start, 4);
    }

    #[test]
    fn keeps_overlong_line_whole() {
        let log = "short\nWAYTOOLONGLINE\nx\n";
        let chunks = chunk_log(log, 4);
        assert!(chunks.iter().any(|c| c.text.contains("WAYTOOLONGLINE")));
        for c in &chunks {
            assert!(c.text.matches("WAYTOOLONGLINE").count() <= 1);
        }
    }

    #[test]
    fn carries_color_prefix_across_boundary() {
        let log = "\x1b[31mred line one\nstill red line two\n";
        let chunks = chunk_log(log, 14);
        assert_eq!(chunks[0].color_prefix, "");
        assert_eq!(chunks[1].color_prefix, "\x1b[31m");
    }

    #[test]
    fn empty_log_yields_no_chunks() {
        assert!(chunk_log("", 256).is_empty());
    }

    #[tokio::test]
    async fn finalize_writes_compressed_chunks_and_descs() {
        use crate::log::{FileLogStorage, LogStorage};
        use gradient_types::ids::BuildAttemptId;
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = BuildAttemptId::new(uuid::Uuid::new_v4());
        let log = "line one\nline two\nline three\n";
        let descs = super::compress_and_store_chunks(&storage, id, log, 12)
            .await
            .unwrap();
        assert!(descs.len() >= 2);
        let raw = storage.read_chunk(id, 0).await.unwrap();
        let decompressed = zstd::stream::decode_all(&raw[..]).unwrap();
        assert_eq!(String::from_utf8(decompressed).unwrap(), descs[0].text);
    }
}
