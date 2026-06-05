/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/** One chunk's metadata from the `/log/chunks` index. */
export interface LogChunkMeta {
  index: number;
  line_start: number;
  line_count: number;
  byte_start: number;
  byte_len: number;
}

export interface LogChunkIndex {
  total_chunks: number;
  total_lines: number;
  total_bytes: number;
  chunks: LogChunkMeta[];
}

export interface LogSearchHit {
  line_number: number;
  chunk_index: number;
  byte_offset: number;
  preview: string;
}

/** Parse a `#L1234` (or `L1234` / `1234`) router fragment into a 1-based line. */
export function parseLineFragment(fragment: string | null | undefined): number | null {
  if (!fragment) return null;
  const m = /^L?(\d+)$/i.exec(fragment.trim());
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isFinite(n) && n > 0 ? n : null;
}

/** Index (0-based) of the chunk containing the given 0-based line, or -1. */
export function chunkIndexForLine(chunks: LogChunkMeta[], line: number): number {
  for (const c of chunks) {
    if (line >= c.line_start && line < c.line_start + c.line_count) {
      return c.index;
    }
  }
  return -1;
}

/**
 * Given the total line count and a target 1-based line, compute the inclusive
 * 1-based [start, end] window of `windowSize` lines centred on the target,
 * clamped to [1, totalLines].
 */
export function windowAround(
  totalLines: number,
  targetLine: number,
  windowSize: number,
): { start: number; end: number } {
  if (totalLines <= 0) return { start: 1, end: 0 };
  const half = Math.floor(windowSize / 2);
  let start = Math.max(1, targetLine - half);
  let end = Math.min(totalLines, start + windowSize - 1);
  start = Math.max(1, end - windowSize + 1);
  return { start, end };
}
