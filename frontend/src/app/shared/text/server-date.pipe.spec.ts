/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ServerTimestampToLocalDateTimePipe } from './server-date.pipe';

describe('ServerTimestampToLocalDateTimePipe', () => {
  const pipe = new ServerTimestampToLocalDateTimePipe();

  beforeEach(() => vi.stubEnv('TZ', 'Asia/Tokyo'));
  afterEach(() => vi.unstubAllEnvs());

  it('shows one format in local time with a 24 hour clock', () => {
    expect(pipe.transform('2026-09-01 15:04:00')).toBe('Sep 2, 2026, 00:04');
    expect(pipe.transform('2026-09-01T15:04:00Z')).toBe('Sep 2, 2026, 00:04');
  });

  it('renders nothing without a timestamp', () => {
    expect(pipe.transform(null)).toBe('');
  });
});
