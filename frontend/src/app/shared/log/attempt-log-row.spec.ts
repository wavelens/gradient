/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptFailureReason, BuildAttemptResponse } from '@core/models';
import { dateTimeText, serverTime } from '@shared/text';
import {
  attemptRowTooltipText,
  buildAttemptResponseToAttemptRow,
  buildExecJobAttemptViewToAttemptLogRow,
  formatAttemptDuration,
} from './attempt-log-row';

const buildAttempt = (outcome: BuildAttemptResponse['outcome'], reason: string | null = null): BuildAttemptResponse => ({
  number: 1, outcome, reason: reason as AttemptFailureReason | null,
  started_at: '2026-01-01T00:00:01', finished_at: '2026-01-01T00:02:33', worker: 'worker-id',
});

describe('buildAttemptResponseToAttemptRow', () => {
  it.each([
    ['Failed', 'WallClockTimeout', 'timed out'],
    ['Failed', 'WorkerLost', 'worker lost'],
    ['Failed', 'Oom', 'out of memory'],
    ['Failed', 'BuilderNonzero', 'failed'],
    ['Failed', 'ReasonOfNewerServer', 'failed'],
    ['Failed', null, 'failed'],
    ['Built', null, 'built'],
    ['Substituted', null, 'substituted'],
    ['Running', null, 'running'],
  ] as const)('attaches a failure reason to failed build attempts and a lowercase label to all other results: %s %s', (outcome, reason, status) => {
    expect(buildAttemptResponseToAttemptRow(buildAttempt(outcome, reason)).status).toBe(status);
  });

  it.each([
    ['Built', 'success'],
    ['Substituted', 'success'],
    ['Failed', 'failure'],
    ['Aborted', 'failure'],
    ['Running', 'running'],
  ] as const)('colour-code Built and Substituted attempts as success, Failed and Aborted attempts as failure, Running attempts as running: %s', (outcome, statusPhase) => {
    expect(buildAttemptResponseToAttemptRow(buildAttempt(outcome)).statusPhase).toBe(statusPhase);
  });
});

describe('buildExecJobAttemptViewToAttemptLogRow', () => {
  it('label attempts of an exec job by end reason and label attempts with no end as running', () => {
    const started = { number: 2, worker: 'w1', started_at: '2026-01-01T00:00:01' };
    expect(buildExecJobAttemptViewToAttemptLogRow({ ...started, ended_at: '2026-01-01T00:00:05', end_reason: 'lost' }, 'gpu-box')).toEqual({
      number: 2, status: 'worker lost', statusPhase: 'failure',
      attemptStartedAt: '2026-01-01T00:00:01', attemptEndedAt: '2026-01-01T00:00:05', attemptWorkerMachineDisplayName: 'gpu-box',
    });
    expect(buildExecJobAttemptViewToAttemptLogRow({ ...started, ended_at: '2026-01-01T00:00:05', end_reason: 'completed' }, null))
      .toMatchObject({ status: 'completed', statusPhase: 'success', attemptWorkerMachineDisplayName: null });
    expect(buildExecJobAttemptViewToAttemptLogRow(started, null)).toMatchObject({ status: 'running', statusPhase: 'running', attemptEndedAt: null });
  });
});

describe('formatAttemptDuration', () => {
  it('measure a completed attempt from start to end and an in-progress attempt up to now', () => {
    const finished = buildAttemptResponseToAttemptRow(buildAttempt('Built'));
    expect(formatAttemptDuration(finished, 0)).toBe('2m 32s');
    const running = { ...finished, attemptEndedAt: null };
    expect(formatAttemptDuration(running, serverTime('2026-01-01T00:00:11').getTime())).toBe('10.0 s');
    expect(formatAttemptDuration({ ...finished, attemptStartedAt: null }, 0)).toBe('');
  });
});

describe('attemptRowTooltipText', () => {
  it('show end time and worker in tooltip and start time during attempt execution', () => {
    const finished = { ...buildAttemptResponseToAttemptRow(buildAttempt('Built')), attemptWorkerMachineDisplayName: 'Dev Worker' };
    expect(attemptRowTooltipText(finished)).toBe(`Finished ${dateTimeText('2026-01-01T00:02:33')} on Dev Worker`);
    expect(attemptRowTooltipText({ ...finished, attemptEndedAt: null })).toBe(`Started ${dateTimeText('2026-01-01T00:00:01')} on Dev Worker`);
    expect(attemptRowTooltipText({ ...finished, attemptWorkerMachineDisplayName: null })).toBe(`Finished ${dateTimeText('2026-01-01T00:02:33')}`);
    expect(attemptRowTooltipText({ ...finished, attemptStartedAt: null, attemptEndedAt: null })).toBe('');
  });
});
