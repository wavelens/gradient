/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { describe, it, expect } from 'vitest';
import { parseLineFragment, windowAround, searchLines, splitLogLines } from './log-window';

describe('splitLogLines', () => {
  it('keeps blank lines and drops only the end of the final newline', () => {
    expect(splitLogLines('a\n\nb\n')).toEqual(['a', '', 'b']);
    expect(splitLogLines('a\npartial')).toEqual(['a', 'partial']);
    expect(splitLogLines('')).toEqual([]);
  });
});

describe('parseLineFragment', () => {
  it('parses #L style fragments', () => {
    expect(parseLineFragment('L1234')).toBe(1234);
    expect(parseLineFragment('42')).toBe(42);
  });
  it('rejects garbage and non-positive', () => {
    expect(parseLineFragment('foo')).toBeNull();
    expect(parseLineFragment('0')).toBeNull();
    expect(parseLineFragment(null)).toBeNull();
  });
});

describe('windowAround', () => {
  it('centres and clamps to bounds', () => {
    expect(windowAround(1000, 500, 100)).toEqual({ start: 450, end: 549 });
    expect(windowAround(1000, 1, 100)).toEqual({ start: 1, end: 100 });
    expect(windowAround(1000, 1000, 100)).toEqual({ start: 901, end: 1000 });
  });
  it('handles empty logs', () => {
    expect(windowAround(0, 1, 100)).toEqual({ start: 1, end: 0 });
  });
});

describe('searchLines', () => {
  it('finds a streamed line case-insensitively, numbered from 1', () => {
    const hits = searchLines(['building', 'Error: boom', 'done', 'another error'], 'error');
    expect(hits.map(h => h.line_number)).toEqual([2, 4]);
    expect(hits[0].preview).toBe('Error: boom');
  });

  it('matches the text a reader sees, not the colour codes around it', () => {
    expect(searchLines(['\x1b[31mfail\x1b[0med'], 'failed')).toHaveLength(1);
  });
});
