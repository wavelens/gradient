/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EvaluationStatus } from '@core/models';
import { isRunningEvaluationStatus } from './duration';
import { isPendingBuildStatus } from './status-phase';

const RETRYABLE_BUILD_STATUSES: ReadonlySet<string> = new Set<BuildStatus>([
  'FailedPermanent',
  'FailedTimeout',
  'Aborted',
  'DependencyFailed',
]);
const REOPENABLE_EVALUATION_STATUSES: ReadonlySet<EvaluationStatus> = new Set<EvaluationStatus>(['Failed', 'Aborted']);

export function canPrioritizeBuild(build: { status: string; prioritized: boolean }): boolean {
  return !build.prioritized && isPendingBuildStatus(build.status);
}

export function canAbortBuild(evaluationStatus: EvaluationStatus, buildStatus: string): boolean {
  return isRunningEvaluationStatus(evaluationStatus) && isPendingBuildStatus(buildStatus);
}

export function canRetryBuild(
  evaluation: { status: EvaluationStatus; replaced: boolean },
  buildStatus: string,
): boolean {
  const open = isRunningEvaluationStatus(evaluation.status)
    || (REOPENABLE_EVALUATION_STATUSES.has(evaluation.status) && !evaluation.replaced);

  return open && RETRYABLE_BUILD_STATUSES.has(buildStatus);
}
