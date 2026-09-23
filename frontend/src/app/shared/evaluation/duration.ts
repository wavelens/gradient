/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EvaluationStatus } from '@core/models';

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

export function formatEvaluationDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export function parseUtcTimestamp(ts: string): number {
  return new Date(ts.includes('Z') || ts.includes('+') ? ts : ts + 'Z').getTime();
}

interface EvaluationTimes {
  status: EvaluationStatus;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  updated_at: string;
}

/// Queue time until the evaluation starts fetching, then time since it did.
export function evaluationDuration(evaluation: EvaluationTimes, nowMs: number): number {
  const queued = evaluation.status === 'Queued';
  const start = parseUtcTimestamp((!queued && evaluation.started_at) || evaluation.created_at);
  const end = isRunningEvaluationStatus(evaluation.status)
    ? nowMs
    : parseUtcTimestamp(evaluation.finished_at ?? evaluation.updated_at);
  return end - start;
}

interface BuildTimes {
  status: string;
  build_time_ms: number | null;
  build_started_at: string | null;
}

export function buildDuration(build: BuildTimes, nowMs: number): number | null {
  if (build.build_time_ms != null) return build.build_time_ms;
  if (build.status !== 'Building' || !build.build_started_at) return null;
  return Math.max(0, nowMs - parseUtcTimestamp(build.build_started_at));
}
