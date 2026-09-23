/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  buildDuration,
  evaluationDuration,
  formatEvaluationDuration,
  isRunningEvaluationStatus,
  parseUtcTimestamp,
} from './duration';

describe('isRunningEvaluationStatus', () => {
  it('returns true for in-flight statuses', () => {
    expect(isRunningEvaluationStatus('Queued')).toBe(true);
    expect(isRunningEvaluationStatus('Fetching')).toBe(true);
    expect(isRunningEvaluationStatus('EvaluatingFlake')).toBe(true);
    expect(isRunningEvaluationStatus('EvaluatingDerivation')).toBe(true);
    expect(isRunningEvaluationStatus('Building')).toBe(true);
    expect(isRunningEvaluationStatus('Waiting')).toBe(true);
  });

  it('returns false for terminal statuses', () => {
    expect(isRunningEvaluationStatus('Completed')).toBe(false);
    expect(isRunningEvaluationStatus('Failed')).toBe(false);
    expect(isRunningEvaluationStatus('Aborted')).toBe(false);
  });
});

describe('formatEvaluationDuration', () => {
  it('shows seconds only when sub-minute', () => {
    expect(formatEvaluationDuration(5_000)).toBe('5s');
  });

  it('shows minutes + seconds when sub-hour', () => {
    expect(formatEvaluationDuration(125_000)).toBe('2m 5s');
  });

  it('shows hours + minutes + seconds when long-running', () => {
    expect(formatEvaluationDuration(3_725_000)).toBe('1h 2m 5s');
  });

  it('clamps negative durations (clock skew) to 0s', () => {
    expect(formatEvaluationDuration(-1_000)).toBe('0s');
  });
});

describe('parseUtcTimestamp', () => {
  it('parses ISO strings with explicit zone', () => {
    expect(parseUtcTimestamp('2026-05-20T12:00:00Z')).toBe(Date.UTC(2026, 4, 20, 12, 0, 0));
    expect(parseUtcTimestamp('2026-05-20T14:00:00+02:00')).toBe(Date.UTC(2026, 4, 20, 12, 0, 0));
  });

  it('defaults to UTC when the timestamp omits a zone', () => {
    // Backend frequently emits naive timestamps; we treat them as UTC so durations
    // are not skewed by the viewer's local offset.
    expect(parseUtcTimestamp('2026-05-20T12:00:00')).toBe(Date.UTC(2026, 4, 20, 12, 0, 0));
  });
});

describe('evaluationDuration', () => {
  const created = '2026-05-20T12:00:00Z';
  const started = '2026-05-20T12:00:40Z';
  const finished = '2026-05-20T12:01:30Z';
  const updated = '2026-05-20T12:03:00Z';
  const now = Date.UTC(2026, 4, 20, 12, 5, 0);
  const at = (status: 'Queued' | 'Fetching' | 'Completed', started_at: string | null, finished_at: string | null) =>
    evaluationDuration({ status, created_at: created, started_at, finished_at, updated_at: updated }, now);

  it('counts the queue while the evaluation waits in it', () => {
    expect(at('Queued', null, null)).toBe(5 * 60 * 1000);
  });

  it('restarts from zero once the evaluation leaves the queue', () => {
    expect(at('Fetching', started, null)).toBe(4 * 60 * 1000 + 20_000);
  });

  it('stops at finished_at, not at a later updated_at', () => {
    expect(at('Completed', started, finished)).toBe(50_000);
  });

  it('falls back to created_at and updated_at on rows without phase stamps', () => {
    expect(at('Completed', null, null)).toBe(3 * 60 * 1000);
  });
});

describe('buildDuration', () => {
  const now = Date.UTC(2026, 4, 20, 12, 5, 0);

  it('is the measured time once the attempt finished', () => {
    expect(buildDuration({ status: 'Completed', build_time_ms: 7_000, build_started_at: null }, now)).toBe(7_000);
  });

  it('runs from the attempt start while building, not from the queue', () => {
    expect(buildDuration({ status: 'Building', build_time_ms: null, build_started_at: '2026-05-20T12:04:00' }, now))
      .toBe(60_000);
  });

  it('is unknown for a build that has not started', () => {
    expect(buildDuration({ status: 'Queued', build_time_ms: null, build_started_at: null }, now)).toBeNull();
  });
});
