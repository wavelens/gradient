/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptEndReason, ExecJobAttemptView, ExecJobResponse } from '@core/models';
import { buildAttemptEndReasonLabel } from '@shared/evaluation';
import { isOpenExecJob } from './exec-job-status';

export interface FailedAttemptWarningBannerContent {
  failedAttemptNumber: number;
  attemptEndReasonDisplayText: string;
  endedAttemptMessageText: string | null;
  consecutiveLostWorkerAttemptCount: number;
}

export const failedAttemptEndReasons: ReadonlySet<AttemptEndReason> = new Set<AttemptEndReason>(['failed', 'timeout', 'lost']);

function endedAttemptsOldestFirst(attempts: readonly ExecJobAttemptView[]): ExecJobAttemptView[] {
  return attempts.filter((attempt) => attempt.end_reason).sort((a, b) => a.number - b.number);
}

export function countNewestConsecutiveLostAttempts(attempts: readonly ExecJobAttemptView[]): number {
  const endedAttemptsNewestFirst = endedAttemptsOldestFirst(attempts).reverse();
  const lostAttemptsBeforeFirstOtherEnd = endedAttemptsNewestFirst.findIndex((attempt) => attempt.end_reason !== 'lost');
  return lostAttemptsBeforeFirstOtherEnd === -1 ? endedAttemptsNewestFirst.length : lostAttemptsBeforeFirstOtherEnd;
}

function displayedAttemptIfEndedElseAttemptBefore(
  attempts: readonly ExecJobAttemptView[],
  displayedAttemptNumber: number | null,
): ExecJobAttemptView | undefined {
  const displayedAttempt = attempts.find((attempt) => attempt.number === displayedAttemptNumber);
  if (displayedAttempt?.end_reason) return displayedAttempt;
  const attemptsBeforeDisplayed = attempts.filter((attempt) => !displayedAttempt || attempt.number < displayedAttempt.number);
  return attemptsBeforeDisplayed.reduce<ExecJobAttemptView | undefined>(
    (newest, attempt) => (!newest || attempt.number > newest.number ? attempt : newest),
    undefined,
  );
}

export function getFailureBannerForDisplayedAttempt(
  job: Pick<ExecJobResponse, 'status' | 'message'>,
  attempts: readonly ExecJobAttemptView[],
  displayedAttemptNumber: number | null,
): FailedAttemptWarningBannerContent | null {
  const attempt = displayedAttemptIfEndedElseAttemptBefore(attempts, displayedAttemptNumber);
  if (!attempt?.end_reason || !failedAttemptEndReasons.has(attempt.end_reason)) return null;
  const jobIsOpen = isOpenExecJob(job.status);
  const isNewestEndedAttempt = attempt.number === endedAttemptsOldestFirst(attempts).at(-1)?.number;
  const message = attempt.message ?? null;
  const jobMessageBannerSaysTheSame = isNewestEndedAttempt && message === job.message;
  if (!jobIsOpen && (!message || jobMessageBannerSaysTheSame)) return null;
  return {
    failedAttemptNumber: attempt.number,
    attemptEndReasonDisplayText: buildAttemptEndReasonLabel(attempt.end_reason).toLowerCase(),
    endedAttemptMessageText: message,
    consecutiveLostWorkerAttemptCount:
      jobIsOpen && isNewestEndedAttempt && attempt.end_reason === 'lost' ? countNewestConsecutiveLostAttempts(attempts) : 0,
  };
}
