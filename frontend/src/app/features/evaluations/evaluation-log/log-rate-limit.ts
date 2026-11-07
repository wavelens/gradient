/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// eslint-disable-next-line no-control-regex -- workers add an ANSI reset at the start of notes
const RATE_LIMIT_NOTE = /^(?:\x1b\[[\d;]*m)*\[gradient: log (paused|resumed|truncated)\b[^\]]*?(?:dropped (\d+) lines)?\]/;

export type RateLimitNote = 'paused' | 'resumed' | 'truncated';

function parse(line: string): { note: RateLimitNote; skipped: string | undefined } | null {
  const match = RATE_LIMIT_NOTE.exec(line);
  return match ? { note: match[1] as RateLimitNote, skipped: match[2] } : null;
}

export function rateLimitMarker(line: string): string | null {
  const parsed = parse(line);
  switch (parsed?.note) {
    case 'paused': return 'Log paused: build output is faster than the rate limit';
    case 'resumed': return `Log resumed: ${parsed.skipped ?? 'some'} lines skipped`;
    case 'truncated': return 'Log cut off: build output became faster than the rate limit';
    default: return null;
  }
}

export function lastRateLimitNote(lines: readonly string[]): RateLimitNote | null {
  for (let i = lines.length - 1; i >= 0; i--) {
    const parsed = parse(lines[i]);
    if (parsed) return parsed.note;
  }
  return null;
}
