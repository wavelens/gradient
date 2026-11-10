/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// Multiple evaluations can share one build. A shared build can continue to run for a newer evaluation after the end of an evaluation. The shared build can also finish for that newer evaluation. These helpers show the state of the build at the end of this evaluation. Later changes leave the view for that evaluation unchanged.

import type { BuildStatusCounts, EvaluationStatus } from '@core/models';
import { formatDuration, serverTime } from '@shared/text';
import { buildDuration, isRunningEvaluationStatus } from './duration';
import { isPendingBuildStatus } from './status-phase';

export interface BuildTimesInEvaluation {
  status: string;
  build_started_at?: string | null;
  build_time_ms?: number | null;
}

export interface EvaluationTimesForBuilds {
  status: EvaluationStatus;
  created_at?: string | null;
  finished_at?: string | null;
}

function buildFinishedAfterEvaluation(build: BuildTimesInEvaluation, evaluation: EvaluationTimesForBuilds): boolean {
  if (evaluation.status !== 'Aborted' || !evaluation.finished_at || !build.build_started_at || build.build_time_ms == null) return false;
  return serverTime(build.build_started_at).getTime() + build.build_time_ms > serverTime(evaluation.finished_at).getTime();
}

export function buildStatusToShowInEvaluation(build: BuildTimesInEvaluation, evaluation: EvaluationTimesForBuilds): string {
  if (isRunningEvaluationStatus(evaluation.status)) return build.status;
  return isPendingBuildStatus(build.status) || buildFinishedAfterEvaluation(build, evaluation) ? 'Aborted' : build.status;
}

export function buildDurationTextForEvaluation(build: BuildTimesInEvaluation, evaluation: EvaluationTimesForBuilds | null, nowMs: number): string {
  if (evaluation && buildStatusToShowInEvaluation(build, evaluation) !== build.status) {
    if (!build.build_started_at || !evaluation.finished_at) return '';
    return formatDuration(Math.max(0, serverTime(evaluation.finished_at).getTime() - serverTime(build.build_started_at).getTime()));
  }
  // A build that started before the evaluation existed served an earlier evaluation. Its build time doesn't count for this evaluation.
  if (!isPendingBuildStatus(build.status) && build.status !== 'Substituted' && build.build_started_at && evaluation?.created_at
    && serverTime(build.build_started_at).getTime() < serverTime(evaluation.created_at).getTime()) return 'reused';
  const ms = buildDuration(build, nowMs);
  return ms == null ? '' : formatDuration(ms);
}

export function countBuildStatusesAfterEvaluationFinished(counts: BuildStatusCounts, evaluationStatus: EvaluationStatus): BuildStatusCounts {
  if (isRunningEvaluationStatus(evaluationStatus) || counts.building + counts.queued === 0) return counts;
  return { ...counts, aborted: counts.aborted + counts.building + counts.queued, building: 0, queued: 0 };
}
