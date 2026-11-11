/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildDuration, evaluationDuration, getEstimatedEvaluationRemainingMilliseconds, isRunningEvaluationStatus } from './duration';

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

describe('evaluationDuration', () => {
  const started = '2026-05-20T12:00:40Z';
  const finished = '2026-05-20T12:01:30Z';
  const updated = '2026-05-20T12:03:00Z';
  const now = Date.UTC(2026, 4, 20, 12, 5, 0);
  const at = (status: 'Queued' | 'Fetching' | 'Completed', started_at: string | null, finished_at: string | null) =>
    evaluationDuration({ status, started_at, finished_at, updated_at: updated }, now);

  it('Queued evaluations have no duration while they wait', () => {
    expect(at('Queued', null, null)).toBeNull();
  });

  it('Running evaluations count from the first fetch.', () => {
    expect(at('Fetching', started, null)).toBe(4 * 60 * 1000 + 20_000);
  });

  it('stops at finished_at, not at a later updated_at', () => {
    expect(at('Completed', started, finished)).toBe(50_000);
  });

  it('Rows without fetch start have no duration', () => {
    expect(at('Completed', null, null)).toBeNull();
  });
});

describe('buildDuration', () => {
  const now = Date.UTC(2026, 4, 20, 12, 5, 0);

  it('is the measured time once the attempt finished', () => {
    expect(buildDuration({ status: 'Completed', build_time_ms: 7_000, build_started_at: null }, now)).toBe(7_000);
  });

  it('counts from the attempt start while building, not from the queue', () => {
    expect(buildDuration({ status: 'Building', build_time_ms: null, build_started_at: '2026-05-20T12:04:00' }, now))
      .toBe(60_000);
  });

  it('is unknown for a build that has not started', () => {
    expect(buildDuration({ status: 'Queued', build_time_ms: null, build_started_at: null }, now)).toBeNull();
  });
});

describe('getEstimatedEvaluationRemainingMilliseconds', () => {
  const now = Date.UTC(2026, 4, 20, 12, 4, 0);
  const running = { status: 'Building' as const, expected_duration_ms: 600_000, phase_started_at: { fetch: '2026-05-20T12:00:00Z' } };

  it('subtracts the time since the fetch started from the expected duration', () => {
    expect(getEstimatedEvaluationRemainingMilliseconds(running, now)).toBe(360_000);
  });

  it('measures from the build start when the fetch time is unknown', () => {
    expect(getEstimatedEvaluationRemainingMilliseconds({ ...running, phase_started_at: { building: '2026-05-20T12:02:00Z' } }, now)).toBe(480_000);
  });

  it('is unknown without an estimate, a start, a running status, or once past the estimate', () => {
    expect(getEstimatedEvaluationRemainingMilliseconds({ ...running, expected_duration_ms: null }, now)).toBeNull();
    expect(getEstimatedEvaluationRemainingMilliseconds({ ...running, phase_started_at: {} }, now)).toBeNull();
    expect(getEstimatedEvaluationRemainingMilliseconds({ ...running, status: 'Completed' }, now)).toBeNull();
    expect(getEstimatedEvaluationRemainingMilliseconds(running, now + 3_600_000)).toBeNull();
  });
});
