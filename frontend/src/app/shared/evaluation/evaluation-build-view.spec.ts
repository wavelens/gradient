/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EvaluationStatus } from '@core/models';
import { buildDurationTextForEvaluation, buildStatusToShowInEvaluation, countBuildStatusesAfterEvaluationFinished } from './evaluation-build-view';

const aborted = { status: 'Aborted' as const, created_at: '2026-05-20T12:00:00', finished_at: '2026-05-20T12:04:00' };

describe('buildStatusToShowInEvaluation', () => {
  it.each<[BuildStatus, EvaluationStatus, BuildStatus]>([
    ['Building', 'Aborted', 'Aborted'],
    ['Queued', 'Failed', 'Aborted'],
    ['Created', 'Completed', 'Aborted'],
    ['FailedPermanent', 'Aborted', 'FailedPermanent'],
    ['Building', 'Building', 'Building'],
  ])('shows %s in a %s evaluation as %s', (status, evaluationStatus, expected) => {
    expect(buildStatusToShowInEvaluation({ status }, { status: evaluationStatus })).toBe(expected);
  });

  it('keeps a shared build aborted after it finishes for a newer evaluation', () => {
    const build = { status: 'Completed', build_started_at: '2026-05-20T12:01:00', build_time_ms: 10 * 60_000 };
    expect(buildStatusToShowInEvaluation(build, aborted)).toBe('Aborted');
    expect(buildStatusToShowInEvaluation({ ...build, build_time_ms: 60_000 }, aborted)).toBe('Completed');
  });
});

describe('buildDurationTextForEvaluation', () => {
  const build = { status: 'Completed', build_time_ms: 3_600_000, build_started_at: '2026-05-20T11:00:00' };
  const evaluation = { status: 'Completed' as const, created_at: '2026-05-20T12:00:00', finished_at: '2026-05-20T13:00:00' };

  it('names a finished build that started before the evaluation existed as reused', () => {
    expect(buildDurationTextForEvaluation(build, evaluation, 0)).toBe('reused');
  });

  it('shows the time of a build the evaluation ran itself', () => {
    expect(buildDurationTextForEvaluation(build, { ...evaluation, created_at: '2026-05-20T10:59:00' }, 0)).toBe('1h 00m');
    expect(buildDurationTextForEvaluation({ status: 'Queued', build_time_ms: null, build_started_at: null }, { ...evaluation, status: 'Building' }, 0)).toBe('');
  });

  it('runs the timer of a shared build that is still building instead of calling it reused', () => {
    const running = { status: 'Building', build_time_ms: null, build_started_at: '2026-05-20T11:59:00' };
    const now = new Date('2026-05-20T12:01:00Z').getTime();
    expect(buildDurationTextForEvaluation(running, { ...evaluation, status: 'Building' }, now)).toBe('2m 00s');
  });

  it('stops the timer of a shared build when the evaluation ended', () => {
    const running = { status: 'Building', build_time_ms: null, build_started_at: '2026-05-20T12:01:00' };
    expect(buildDurationTextForEvaluation(running, aborted, Date.now())).toBe('3m 00s');
    expect(buildDurationTextForEvaluation({ ...running, status: 'Completed', build_time_ms: 10 * 60_000 }, aborted, 0)).toBe('3m 00s');
  });
});

describe('countBuildStatusesAfterEvaluationFinished', () => {
  const counts = { completed: 1, failed: 0, building: 2, queued: 3, substituted: 0, aborted: 1, dependency_failed: 0 };

  it('moves unfinished builds to aborted once the evaluation finished', () => {
    expect(countBuildStatusesAfterEvaluationFinished(counts, 'Aborted')).toEqual({ ...counts, building: 0, queued: 0, aborted: 6 });
  });

  it('keeps the counts of a running evaluation', () => {
    expect(countBuildStatusesAfterEvaluationFinished(counts, 'Building')).toBe(counts);
  });
});
