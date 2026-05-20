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

export function evaluationDuration(
  evaluation: { status: EvaluationStatus; created_at: string; updated_at: string },
  nowMs: number,
): number {
  const start = parseUtcTimestamp(evaluation.created_at);
  const end = isRunningEvaluationStatus(evaluation.status) ? nowMs : parseUtcTimestamp(evaluation.updated_at);
  return end - start;
}
