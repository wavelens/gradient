/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildDuration, buildDurationText, evaluationDuration, isRunningEvaluationStatus } from './duration';

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

describe('buildDurationText', () => {
  const build = { status: 'Completed', build_time_ms: 3_600_000, build_started_at: '2026-05-20T11:00:00' };

  it('names a build that started before the evaluation existed as reused instead of showing its old time', () => {
    expect(buildDurationText(build, 0, '2026-05-20T12:00:00')).toBe('reused');
  });

  it('shows the time of a build the evaluation ran itself', () => {
    expect(buildDurationText(build, 0, '2026-05-20T10:59:00')).not.toBe('reused');
    expect(buildDurationText({ ...build, build_time_ms: null, status: 'Queued', build_started_at: null }, 0, '2026-05-20T10:59:00')).toBe('');
  });
});
