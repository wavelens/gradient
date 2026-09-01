/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { relativeTime } from './relative-time';

const NOW = new Date('2026-09-01T12:00:00Z').getTime();
const ago = (ms: number) => new Date(NOW - ms).toISOString();

describe('relativeTime', () => {
  it('says never for a missing timestamp', () => {
    expect(relativeTime(null, NOW)).toBe('never');
    expect(relativeTime(undefined, NOW)).toBe('never');
  });

  it('counts seconds under a minute', () => {
    expect(relativeTime(ago(20_000), NOW)).toBe('20s ago');
  });

  it('counts minutes under an hour', () => {
    expect(relativeTime(ago(5 * 60_000), NOW)).toBe('5m ago');
  });

  it('counts hours under a day', () => {
    expect(relativeTime(ago(7 * 3_600_000), NOW)).toBe('7h ago');
  });

  it('counts days under a month', () => {
    expect(relativeTime(ago(3 * 86_400_000), NOW)).toBe('3d ago');
  });

  it('counts months under a year', () => {
    expect(relativeTime(ago(70 * 86_400_000), NOW)).toBe('2mo ago');
  });

  it('counts years beyond that', () => {
    expect(relativeTime(ago(400 * 86_400_000), NOW)).toBe('1y ago');
  });

  it('reads a space-separated timestamp as UTC, like the API sends', () => {
    expect(relativeTime('2026-09-01 11:00:00', NOW)).toBe('1h ago');
  });

  it('treats a future timestamp as just now rather than counting backwards', () => {
    expect(relativeTime(new Date(NOW + 60_000).toISOString(), NOW)).toBe('just now');
  });

  it('returns the fallback for an unparseable value', () => {
    expect(relativeTime('not-a-date', NOW)).toBe('never');
  });
});
