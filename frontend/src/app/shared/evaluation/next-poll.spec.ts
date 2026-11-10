/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { getNextPollTime, nextPollText } from './next-poll';

const NOW = Date.parse('2026-11-10T12:00:00Z');

describe('next poll', () => {
  const polling = { type: 'polling' as const, active: true, created_at: '2026-11-10 11:00:00', config: { interval_secs: 300 } };

  it('adds the interval to the last firing, else to the creation', () => {
    expect(getNextPollTime({ ...polling, last_fired_at: '2026-11-10 11:58:00' })).toBe(Date.parse('2026-11-10T12:03:00Z'));
    expect(getNextPollTime(polling)).toBe(Date.parse('2026-11-10T11:05:00Z'));
  });

  it('has no next poll for paused or non-polling triggers', () => {
    expect(getNextPollTime({ ...polling, active: false })).toBeNull();
    expect(getNextPollTime({ ...polling, type: 'time', config: { cron: '0 0 3 * * *' } })).toBeNull();
  });

  it('reads as minutes until the poll and never as a past time', () => {
    expect(nextPollText('2026-11-10 12:01:30', NOW)).toBe('Next poll in 2m');
    expect(nextPollText('2026-11-10 13:30:00', NOW)).toBe('Next poll in 1h 30m');
    expect(nextPollText('2026-11-10 11:00:00', NOW)).toBe('Next poll in 1m');
    expect(nextPollText(null, NOW)).toBeNull();
  });
});
