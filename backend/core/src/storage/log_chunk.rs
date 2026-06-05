/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Splits a completed build log into line-bounded chunks for compression and
//! lazy serving. Each chunk records the SGR state active at its first byte so
//! it can be rendered standalone (see [`super::sgr::SgrState`]).

use super::sgr::SgrState;

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
}
