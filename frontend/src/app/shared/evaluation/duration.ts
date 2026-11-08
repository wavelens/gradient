/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EvaluationStatus } from '@core/models';
import { serverTime } from '@shared/text';

const RUNNING_STATUSES: ReadonlySet<EvaluationStatus> = new Set([
  'Queued',
  'Fetching',
  'EvaluatingFlake',
  'EvaluatingDerivation',
  'Building',
  'Waiting',
]);

export function isRunningEvaluationStatus(status: EvaluationStatus): boolean {
  return RUNNING_STATUSES.has(status);
}

interface EvaluationTimes {
  status: EvaluationStatus;
  started_at?: string | null;
  finished_at?: string | null;
  updated_at: string;
}

/// Duration from first fetch to evaluation completion. Null if no server has taken the evaluation from the queue yet.
export function evaluationDuration(evaluation: EvaluationTimes, nowMs: number): number | null {
  if (!evaluation.started_at) return null;
  const end = isRunningEvaluationStatus(evaluation.status)
    ? nowMs
    : serverTime(evaluation.finished_at ?? evaluation.updated_at).getTime();
  return Math.max(0, end - serverTime(evaluation.started_at).getTime());
}

interface BuildTimes {
  status: string;
  build_time_ms?: number | null;
  build_started_at?: string | null;
}

export function buildDuration(build: BuildTimes, nowMs: number): number | null {
  if (build.build_time_ms != null) return build.build_time_ms;
  if (build.status !== 'Building' || !build.build_started_at) return null;
  return Math.max(0, nowMs - serverTime(build.build_started_at).getTime());
}
