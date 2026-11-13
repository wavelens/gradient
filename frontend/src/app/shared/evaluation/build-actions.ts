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

const BUILD_OUTPUT_STATUSES: ReadonlySet<string> = new Set<BuildStatus>(['Completed', 'Substituted']);
const BUILD_STATUSES_WITHOUT_LOG: ReadonlySet<string> = new Set<BuildStatus>(['Created', 'Queued']);

export function getClosureMenuItemDisabledReason(buildStatus: string): string | null {
  return BUILD_OUTPUT_STATUSES.has(buildStatus) ? null : 'The closure is available after the build succeeds.';
}

export function getSbomDownloadMenuItemDisabledReason(buildStatus: string): string | null {
  return BUILD_OUTPUT_STATUSES.has(buildStatus) ? null : 'The SBOM is available after a successful build.';
}

export function getArtefactsMenuItemDisabledReason(buildStatus: string, hasArtefacts: boolean): string | null {
  if (!BUILD_OUTPUT_STATUSES.has(buildStatus)) return 'Artefacts are available after a successful build.';
  return hasArtefacts ? null : 'No stored artefacts for this build.';
}

export function getDownloadLogMenuItemDisabledReason(buildStatus: string): string | null {
  if (BUILD_STATUSES_WITHOUT_LOG.has(buildStatus)) return 'Logs are available after build start.';
  return buildStatus === 'DependencyFailed' ? 'Skipped - a dependency failed, so this build has no log.' : null;
}

export function showJobMenuItemDisabledReason(job: unknown, signedIn: boolean, subject: 'build' | 'evaluation'): string | null {
  if (!signedIn) return 'Sign in to open the job board.';
  return job ? null : `No job for this ${subject} on the job board.`;
}

export function canPrioritizeBuild(build: { status: string; prioritized: boolean }): boolean {
  return !build.prioritized && isPendingBuildStatus(build.status);
}

export function canAbortBuild(evaluationStatus: EvaluationStatus, buildStatus: string): boolean {
  return isRunningEvaluationStatus(evaluationStatus) && isPendingBuildStatus(buildStatus);
}

export function isRetryableBuildStatus(buildStatus: string): boolean {
  return RETRYABLE_BUILD_STATUSES.has(buildStatus);
}

export function canRetryBuild(evaluationStatus: EvaluationStatus, buildStatus: string): boolean {
  return isRunningEvaluationStatus(evaluationStatus) && isRetryableBuildStatus(buildStatus);
}
