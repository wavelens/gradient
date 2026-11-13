/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptEndReason, ExecJobAttemptView } from '@core/models';
import { countNewestConsecutiveLostAttempts, getFailureBannerForDisplayedAttempt } from './exec-job-failed-attempt-warning';

const GUEST_AGENT_GONE = 'the guest agent closed the connection before the program exited';

function ended(number: number, end_reason: AttemptEndReason, message: string | null = null): ExecJobAttemptView {
  return { number, worker: 'w1', started_at: '2026-01-01T00:00:01', ended_at: '2026-01-01T00:03:01', end_reason, message };
}

function running(number: number): ExecJobAttemptView {
  return { number, worker: 'w1', started_at: '2026-01-01T00:04:01' };
}

describe('getFailureBannerForDisplayedAttempt', () => {
  it('describe the failed attempt whose log the page shows', () => {
    const attempts = [ended(1, 'timeout', 'no output for 600 seconds'), running(2)];
    expect(getFailureBannerForDisplayedAttempt({ status: 'running' }, attempts, 1)).toEqual({
      failedAttemptNumber: 1,
      attemptEndReasonDisplayText: 'timed out',
      endedAttemptMessageText: 'no output for 600 seconds',
      consecutiveLostWorkerAttemptCount: 0,
    });
  });

  it('describe the failed previous attempt at the start of a new attempt', () => {
    const attempts = [running(2), ended(1, 'failed', 'the program exited with 1')];
    expect(getFailureBannerForDisplayedAttempt({ status: 'running' }, attempts, 2)).toEqual({
      failedAttemptNumber: 1,
      attemptEndReasonDisplayText: 'failed',
      endedAttemptMessageText: 'the program exited with 1',
      consecutiveLostWorkerAttemptCount: 0,
    });
  });

  it('Describe the latest attempt while waiting for the job to start again.', () => {
    expect(getFailureBannerForDisplayedAttempt({ status: 'queued' }, [ended(1, 'lost')], null)).toEqual({
      failedAttemptNumber: 1,
      attemptEndReasonDisplayText: 'worker lost',
      endedAttemptMessageText: null,
      consecutiveLostWorkerAttemptCount: 1,
    });
  });

  it('Return nothing when the previous attempt aborted or completed right before the running attempt.', () => {
    for (const reason of ['completed', 'aborted'] as const) {
      const attempts = [ended(1, 'lost', GUEST_AGENT_GONE), ended(2, reason), running(3)];
      expect(getFailureBannerForDisplayedAttempt({ status: 'running' }, attempts, 3)).toBeNull();
    }
  });

  it('Count consecutive worker losses only for the latest finished attempt of an open job', () => {
    const attempts = [ended(1, 'failed'), ended(2, 'lost', GUEST_AGENT_GONE), ended(3, 'lost', GUEST_AGENT_GONE), running(4)];
    const countFor = (status: 'running' | 'failed', displayedAttemptNumber: number) =>
      getFailureBannerForDisplayedAttempt({ status, message: 'the exec job lost its worker too many times in a row' }, attempts, displayedAttemptNumber)
        ?.consecutiveLostWorkerAttemptCount;
    expect(countFor('running', 4)).toBe(2);
    expect(countFor('running', 3)).toBe(2);
    expect(countFor('running', 2)).toBe(0);
    expect(countFor('failed', 3)).toBe(0);
  });

  it('Return nothing if the job message already said the same thing about the latest attempt.', () => {
    const attempts = [ended(1, 'failed', 'the program exited with 1')];
    expect(getFailureBannerForDisplayedAttempt({ status: 'failed', message: 'the program exited with 1' }, attempts, 1)).toBeNull();
    expect(getFailureBannerForDisplayedAttempt({ status: 'failed' }, [ended(1, 'failed')], 1)).toBeNull();
  });

  it('Keep attempt message when job messages differ', () => {
    const attempts = [ended(1, 'lost', GUEST_AGENT_GONE)];
    const jobFailedAfterLosses = { status: 'failed', message: 'the exec job lost its worker too many times in a row' } as const;
    expect(getFailureBannerForDisplayedAttempt(jobFailedAfterLosses, attempts, 1)?.endedAttemptMessageText).toBe(GUEST_AGENT_GONE);
  });
});

describe('countNewestConsecutiveLostAttempts', () => {
  it('Count the latest finished attempts in a row that lost a worker.', () => {
    expect(countNewestConsecutiveLostAttempts([running(4), ended(3, 'lost'), ended(1, 'lost'), ended(2, 'failed')])).toBe(1);
    expect(countNewestConsecutiveLostAttempts([ended(1, 'lost'), ended(2, 'lost')])).toBe(2);
    expect(countNewestConsecutiveLostAttempts([ended(1, 'lost'), ended(2, 'completed')])).toBe(0);
    expect(countNewestConsecutiveLostAttempts([])).toBe(0);
  });
});
