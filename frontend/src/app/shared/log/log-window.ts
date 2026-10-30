/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { LogSearchHit } from '@core/models';

// Nix strips carriage returns from build logs; exec jobs pass the guest output through untouched.
function lastRewrite(line: string): string {
  const text = line.replace(/\r+$/, '');
  return text.slice(text.lastIndexOf('\r') + 1);
}

export function splitLogLines(text: string): string[] {
  const lines = text.split('\n');
  if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();
  return lines.map(lastRewrite);
}

export function parseLineFragment(fragment: string | null | undefined): number | null {
  if (!fragment) return null;
  const m = /^L?(\d+)$/i.exec(fragment.trim());
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isFinite(n) && n > 0 ? n : null;
}

export function windowAround(
  totalLines: number,
  targetLine: number,
  windowSize: number,
): { start: number; end: number } {
  if (totalLines <= 0) return { start: 1, end: 0 };
  const half = Math.floor(windowSize / 2);
  let start = Math.max(1, targetLine - half);
  const end = Math.min(totalLines, start + windowSize - 1);
  start = Math.max(1, end - windowSize + 1);
  return { start, end };
}

// eslint-disable-next-line no-control-regex -- ESC is the sequence introducer
const ANSI_ESCAPE = /\u001b\[[?0-9;]*[A-Za-z~]/g;

export function searchLines(lines: readonly string[], query: string): LogSearchHit[] {
  const needle = query.toLowerCase();
  const hits: LogSearchHit[] = [];
  lines.forEach((line, i) => {
    const text = line.replace(ANSI_ESCAPE, '');
    if (text.toLowerCase().includes(needle)) {
      hits.push({ line_number: i + 1, chunk_index: 0, byte_offset: 0, preview: text });
    }
  });

  return hits;
}
