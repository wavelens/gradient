/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { clockTime, monthDay } from './clock-time';

describe('clock time of a server timestamp', () => {
  beforeEach(() => vi.stubEnv('TZ', 'Asia/Tokyo'));
  afterEach(() => vi.unstubAllEnvs());

  it('shows the hour and minute on the browser clock, not in UTC', () => {
    expect(clockTime('2026-09-29T15:04:00Z')).toBe('00:04');
    expect(clockTime(Date.parse('2026-09-29T15:04:00Z'))).toBe('00:04');
  });

  it('reads a timestamp without a zone as UTC', () => {
    expect(clockTime('2026-09-29 15:04:00')).toBe('00:04');
    expect(clockTime('2026-09-29T15:04:00.123456')).toBe('00:04');
  });

  it('moves the date along with the clock', () => {
    expect(monthDay('2026-09-29T15:04:00Z')).toBe('09-30');
  });
});
