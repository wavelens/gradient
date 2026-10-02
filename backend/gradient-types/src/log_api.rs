/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChunkMeta {
    pub index: u32,
    pub line_start: u64,
    pub line_count: u32,
    pub byte_start: u64,
    pub byte_len: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChunkIndex {
    pub total_chunks: u32,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub chunks: Vec<LogChunkMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearchHit {
    pub line_number: u64,
    pub chunk_index: u32,
    pub byte_offset: u64,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearchDone {
    pub done: bool,
    pub total_matches: u64,
}
