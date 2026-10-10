/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::sgr::SgrState;
use crate::log::LogStorage;
use anyhow::Result;
use gradient_types::constants::LOG_CHUNK_ZSTD_LEVEL;
use gradient_types::ids::BuildAttemptId;

pub struct LogChunkDesc {
    pub text: String,
    pub byte_start: u64,
    pub line_start: u64,
    pub line_count: u32,
    pub color_prefix: String,
}

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

pub struct StoredChunkDesc {
    pub text: String,
    pub byte_start: u64,
    pub byte_len: u32,
    pub line_start: u64,
    pub line_count: u32,
    pub compressed_size: u32,
    pub color_prefix: String,
}

pub const MISSING_CHUNK_LINE: &str = "[log chunk unavailable]\n";

/// Only a missing chunk is read as `MISSING_CHUNK_LINE`. A caller rewriting the log must never drop
/// a chunk it merely failed to read.
pub async fn read_chunks(
    storage: &dyn LogStorage,
    log_key: BuildAttemptId,
    count: u32,
) -> Result<String> {
    let mut out = String::new();
    for index in 0..count {
        match storage.read_chunk(log_key, index).await {
            Ok(raw) => out.push_str(&String::from_utf8_lossy(&zstd::stream::decode_all(
                &raw[..],
            )?)),
            Err(e) if crate::log::is_not_found(&e) => out.push_str(MISSING_CHUNK_LINE),
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

pub struct ChunkSpan {
    pub index: u32,
    pub byte_start: u64,
    pub byte_len: u32,
    pub line_count: u32,
}

pub async fn read_chunk_text(
    storage: &dyn LogStorage,
    log_key: BuildAttemptId,
    index: u32,
    line_count: u32,
) -> Result<String> {
    match storage.read_chunk(log_key, index).await {
        Ok(raw) => Ok(String::from_utf8_lossy(&zstd::stream::decode_all(&raw[..])?).into_owned()),
        Err(e) if crate::log::is_not_found(&e) => {
            Ok(MISSING_CHUNK_LINE.repeat(line_count as usize))
        }
        Err(e) => Err(e),
    }
}

pub async fn read_chunks_from(
    storage: &dyn LogStorage,
    log_key: BuildAttemptId,
    chunks: &[ChunkSpan],
    offset: u64,
) -> Result<(String, u64)> {
    let mut text = String::new();
    let mut end = offset;
    for chunk in chunks {
        let chunk_end = chunk.byte_start + chunk.byte_len as u64;
        if chunk_end <= offset {
            continue;
        }

        let chunk_text = read_chunk_text(storage, log_key, chunk.index, chunk.line_count).await?;
        let skip = offset.saturating_sub(chunk.byte_start) as usize;
        text.push_str(chunk_text.get(skip..).unwrap_or_default());
        end = chunk_end;
    }

    Ok((text, end))
}

pub async fn compress_and_store_chunks(
    storage: &dyn LogStorage,
    log_key: BuildAttemptId,
    log: &str,
    target_bytes: usize,
) -> Result<Vec<StoredChunkDesc>> {
    storage.delete_chunks(log_key).await.ok();
    let mut out = Vec::new();
    for (index, c) in chunk_log(log, target_bytes).into_iter().enumerate() {
        let compressed = zstd::stream::encode_all(c.text.as_bytes(), LOG_CHUNK_ZSTD_LEVEL)?;
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
    async fn read_chunks_marks_a_missing_chunk_and_keeps_the_rest() {
        use crate::log::{FileLogStorage, LogStorage};
        use gradient_types::ids::BuildAttemptId;
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = BuildAttemptId::now_v7();
        for (index, text) in [(0, "first\n"), (2, "third\n")] {
            let raw = zstd::stream::encode_all(text.as_bytes(), 1).unwrap();
            storage.write_chunk(id, index, &raw).await.unwrap();
        }

        let text = super::read_chunks(&storage, id, 3).await.unwrap();

        assert_eq!(text, format!("first\n{}third\n", super::MISSING_CHUNK_LINE));
    }

    #[tokio::test]
    async fn finalize_writes_compressed_chunks_and_descs() {
        use crate::log::{FileLogStorage, LogStorage};
        use gradient_types::ids::BuildAttemptId;
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = BuildAttemptId::new(uuid::Uuid::now_v7());
        let log = "line one\nline two\nline three\n";
        let descs = super::compress_and_store_chunks(&storage, id, log, 12)
            .await
            .unwrap();
        assert!(descs.len() >= 2);
        let raw = storage.read_chunk(id, 0).await.unwrap();
        let decompressed = zstd::stream::decode_all(&raw[..]).unwrap();
        assert_eq!(String::from_utf8(decompressed).unwrap(), descs[0].text);
    }

    #[tokio::test]
    async fn read_chunks_from_reads_only_what_follows_the_offset() {
        use crate::log::{FileLogStorage, LogStorage};
        use gradient_types::ids::BuildAttemptId;
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = BuildAttemptId::now_v7();
        let mut chunks = Vec::new();
        let mut byte_start = 0;
        for (index, text) in ["first\n", "second\n"].iter().enumerate() {
            let raw = zstd::stream::encode_all(text.as_bytes(), 1).unwrap();
            storage.write_chunk(id, index as u32, &raw).await.unwrap();
            chunks.push(super::ChunkSpan {
                index: index as u32,
                byte_start,
                byte_len: text.len() as u32,
                line_count: 1,
            });

            byte_start += text.len() as u64;
        }

        let cases = [
            (0, "first\nsecond\n"),
            (3, "st\nsecond\n"),
            (6, "second\n"),
            (13, ""),
        ];

        for (offset, expected) in cases {
            let (text, end) = super::read_chunks_from(&storage, id, &chunks, offset)
                .await
                .unwrap();

            assert_eq!((text.as_str(), end), (expected, 13), "offset {offset}");
        }
    }
}
