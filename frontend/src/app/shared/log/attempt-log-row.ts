/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptFailureReason, AttemptOutcome, BuildAttemptResponse, ExecJobAttemptView } from '@core/models';
import { buildAttemptEndReasonLabel, type StatusPhase } from '@shared/evaluation';
import { dateTimeText, formatDuration, serverTime } from '@shared/text';

export interface AttemptLogRow {
  number: number;
  status: string;
  statusPhase: StatusPhase;
  attemptStartedAt?: string | null;
  attemptEndedAt?: string | null;
  attemptWorkerMachineDisplayName?: string | null;
}

const ATTEMPT_FAILURE_REASON_ROW_TEXT: Partial<Record<AttemptFailureReason, string>> = {
  SubstituteUnavailable: 'substitute unavailable',
  Oom: 'out of memory',
  DiskFull: 'disk full',
  Network: 'network error',
  BuilderCrash: 'builder crashed',
  WallClockTimeout: 'Timed out',
  SilentTimeout: 'no output timeout',
  InputsUnavailable: 'inputs unavailable',
  WorkerLost: 'Worker lost',
  NotEnoughResources: 'not enough resources',
};

const statusPhaseByAttemptOutcome: Record<AttemptOutcome, StatusPhase> = {
  Running: 'running',
  Built: 'success',
  Substituted: 'success',
  Failed: 'failure',
  Aborted: 'failure',
};

// The attempts endpoint can return a worker as an ID without a name. Rows for build attempts then include no worker.
export function buildAttemptResponseToAttemptRow(attempt: BuildAttemptResponse): AttemptLogRow {
  const attemptFailureReasonWords = attempt.outcome === 'Failed' && attempt.reason ? ATTEMPT_FAILURE_REASON_ROW_TEXT[attempt.reason] : undefined;
  return {
    number: attempt.number,
    status: (attemptFailureReasonWords ?? attempt.outcome).toLowerCase(),
    statusPhase: statusPhaseByAttemptOutcome[attempt.outcome] ?? 'failure',
    attemptStartedAt: attempt.started_at ?? null,
    attemptEndedAt: attempt.finished_at ?? null,
    attemptWorkerMachineDisplayName: null,
  };
}

export function buildExecJobAttemptViewToAttemptLogRow(attempt: ExecJobAttemptView, attemptWorkerMachineDisplayName: string | null): AttemptLogRow {
  const attemptEndReason = attempt.end_reason;
  return {
    number: attempt.number,
    status: attemptEndReason ? buildAttemptEndReasonLabel(attemptEndReason).toLowerCase() : 'running',
    statusPhase: !attemptEndReason ? 'running' : attemptEndReason === 'completed' ? 'success' : 'failure',
    attemptStartedAt: attempt.started_at,
    attemptEndedAt: attempt.ended_at ?? null,
    attemptWorkerMachineDisplayName,
  };
}

export function formatAttemptDuration(row: AttemptLogRow, currentTimeMilliseconds: number): string {
  if (!row.attemptStartedAt) return '';
  const attemptEndOrNowMilliseconds = row.attemptEndedAt ? serverTime(row.attemptEndedAt).getTime() : currentTimeMilliseconds;
  return formatDuration(Math.max(0, attemptEndOrNowMilliseconds - serverTime(row.attemptStartedAt).getTime()));
}

export function attemptRowTooltipText(row: AttemptLogRow): string {
  const workerMachineNameSuffix = row.attemptWorkerMachineDisplayName ? ` on ${row.attemptWorkerMachineDisplayName}` : '';
  if (row.attemptEndedAt) return `Finished ${dateTimeText(row.attemptEndedAt)}${workerMachineNameSuffix}`;
  return row.attemptStartedAt ? `Started ${dateTimeText(row.attemptStartedAt)}${workerMachineNameSuffix}` : '';
}
