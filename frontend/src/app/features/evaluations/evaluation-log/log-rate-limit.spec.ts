/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { lastRateLimitNote, rateLimitMarker } from './log-rate-limit';

const PAUSED = '\x1b[0m[gradient: log paused, the build can write output faster than the rate limit for the log]';
const RESUMED = '\x1b[0m[gradient: log resumed, the rate limit for the log dropped 1234 lines]';
const OLD_TRUNCATED = '\x1b[0m[gradient: log truncated — rate limit exceeded]';

describe('log rate limit notes', () => {
  it('turns the worker notes into separators and leaves build output alone', () => {
    expect(rateLimitMarker(PAUSED)).toBe('Log paused: build output is faster than the rate limit');
    expect(rateLimitMarker(RESUMED)).toBe('Log resumed: 1234 lines skipped');
    expect(rateLimitMarker(OLD_TRUNCATED)).toBe('Log cut off: build output became faster than the rate limit');
    expect(rateLimitMarker('echo [gradient: log paused]')).toBeNull();
    expect(rateLimitMarker('building ok')).toBeNull();
  });

  it('reads the newest note to tell whether the log is paused now', () => {
    expect(lastRateLimitNote(['a', PAUSED, 'b'])).toBe('paused');
    expect(lastRateLimitNote([PAUSED, RESUMED, 'c'])).toBe('resumed');
    expect(lastRateLimitNote(['a', 'b'])).toBeNull();
  });
});
