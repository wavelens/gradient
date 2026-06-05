/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { describe, it, expect } from 'vitest';
import {
  parseLineFragment,
  chunkIndexForLine,
  windowAround,
  LogChunkMeta,
} from './log-window';

const chunks: LogChunkMeta[] = [
  { index: 0, line_start: 0, line_count: 10, byte_start: 0, byte_len: 100 },
  { index: 1, line_start: 10, line_count: 10, byte_start: 100, byte_len: 100 },
  { index: 2, line_start: 20, line_count: 5, byte_start: 200, byte_len: 40 },
];

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

describe('chunkIndexForLine', () => {
  it('maps lines to chunks', () => {
    expect(chunkIndexForLine(chunks, 0)).toBe(0);
    expect(chunkIndexForLine(chunks, 9)).toBe(0);
    expect(chunkIndexForLine(chunks, 10)).toBe(1);
    expect(chunkIndexForLine(chunks, 24)).toBe(2);
  });
  it('returns -1 out of range', () => {
    expect(chunkIndexForLine(chunks, 25)).toBe(-1);
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
