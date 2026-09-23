/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { formatBytes, formatCount, formatDuration, formatMegabytes, formatPercent } from './units';

describe('formatBytes', () => {
  it('picks the largest binary unit that keeps the number readable', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1536)).toBe('1.5 KiB');
    expect(formatBytes(5 * 1024 ** 3)).toBe('5.0 GiB');
    expect(formatBytes(3.2 * 1024 ** 4)).toBe('3.2 TiB');
  });

  it('drops the decimal once three digits carry the precision', () => {
    expect(formatBytes(734 * 1024 ** 2)).toBe('734 MiB');
  });

  it('shows a dash for a missing value', () => {
    expect(formatBytes(null)).toBe('-');
    expect(formatBytes(undefined)).toBe('-');
  });
});

describe('formatMegabytes', () => {
  it('reads a worker RAM figure in the unit that fits', () => {
    expect(formatMegabytes(64_000)).toBe('62.5 GiB');
    expect(formatMegabytes(512)).toBe('512 MiB');
  });
});

describe('formatDuration', () => {
  it('scales from milliseconds up to days', () => {
    expect(formatDuration(850)).toBe('850 ms');
    expect(formatDuration(12_340)).toBe('12.3 s');
    expect(formatDuration(245_000)).toBe('4m 05s');
    expect(formatDuration(3_720_000)).toBe('1h 02m');
    expect(formatDuration(2 * 86_400_000 + 3_600_000)).toBe('2d 1h');
  });

  it('shows a dash for a missing value', () => {
    expect(formatDuration(null)).toBe('-');
  });
});

describe('formatCount', () => {
  it('abbreviates large counts', () => {
    expect(formatCount(999)).toBe('999');
    expect(formatCount(12_345)).toBe('12.3k');
    expect(formatCount(4_560_000)).toBe('4.56M');
  });
});

describe('formatPercent', () => {
  it('rounds a ratio to a readable share', () => {
    expect(formatPercent(0.1234)).toBe('12.3 %');
    expect(formatPercent(null)).toBe('-');
  });
});
