/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
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
  const updated = '2026-05-20T12:01:30Z';
  const now = Date.UTC(2026, 4, 20, 12, 5, 0);

  it('uses updated_at for terminal evaluations so the duration stops growing', () => {
    const ms = evaluationDuration(
      { status: 'Completed', created_at: created, updated_at: updated },
      now,
    );
    expect(ms).toBe(90_000);
  });

  it('uses the current time for running evaluations', () => {
    const ms = evaluationDuration(
      { status: 'Building', created_at: created, updated_at: updated },
      now,
    );
    expect(ms).toBe(5 * 60 * 1000);
  });
});
