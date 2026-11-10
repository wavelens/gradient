/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptFailureReason, AttemptOutcome, BuildAttemptLogRow } from '@core/models';
import { formatDuration, serverTime } from '@shared/text';

const outcomeText: Record<AttemptOutcome, string> = {
  Running: 'Running',
  Built: 'Built',
  Substituted: 'From cache',
  Failed: 'Failed',
  Aborted: 'Aborted',
};

const reasonText: Record<AttemptFailureReason, string> = {
  SubstituteUnavailable: 'cache copy unavailable',
  Oom: 'out of memory',
  DiskFull: 'out of disk space',
  Network: 'network error',
  BuilderCrash: 'builder crashed',
  BuilderNonzero: 'build command failed',
  WallClockTimeout: 'timeout',
  SilentTimeout: 'no output for a long time',
  InputsUnavailable: 'input unavailable',
  WorkerLost: 'worker lost',
  NotEnoughResources: 'insufficient resources',
};

export function buildAttemptOutcomeText(attempt: BuildAttemptLogRow): string {
  const outcome = outcomeText[attempt.outcome];
  return attempt.reason ? `${outcome}: ${reasonText[attempt.reason]}` : outcome;
}

export function formatBuildAttemptDurationText(attempt: BuildAttemptLogRow): string {
  if (!attempt.started_at || !attempt.finished_at) return '';
  return formatDuration(Math.max(0, serverTime(attempt.finished_at).getTime() - serverTime(attempt.started_at).getTime()));
}
