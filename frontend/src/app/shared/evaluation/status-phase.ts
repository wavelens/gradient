/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type {
  BuildStatus,
  EvaluationStatus,
  InputFetchState,
  EntryPointSummary,
  ExecJobStatus,
  WaitingReason,
} from '@core/models';
import type { BadgeSeverity } from '@gradient/ui/ui';
import { isRunningEvaluationStatus } from './duration';

export type StatusPhase = 'queued' | 'waiting' | 'running' | 'success' | 'failure' | 'aborted';

/// All pages render each status phase with one color and one badge severity. Failure, running and waiting jobs look the same everywhere as a result.
export const PHASE_COLOR: Record<StatusPhase, string> = {
  queued: 'var(--gr-graph-queued)',
  running: 'var(--gr-graph-running)',
  waiting: 'var(--gr-status-warning)',
  success: 'var(--gr-graph-success)',
  failure: 'var(--gr-graph-danger)',
  aborted: 'var(--gr-text-secondary)',
};

export const PHASE_SEVERITY: Record<StatusPhase, BadgeSeverity> = {
  queued: 'neutral',
  running: 'info',
  waiting: 'warning',
  success: 'success',
  failure: 'danger',
  aborted: 'neutral',
};

export function evaluationPhase(status: EvaluationStatus, waitingReason?: WaitingReason | null): StatusPhase {
  const phase = statusToDisplayPhase(status);
  return waitingReason && (phase === 'queued' || phase === 'running') ? 'waiting' : phase;
}

function statusToDisplayPhase(status: EvaluationStatus): StatusPhase {
  switch (status) {
    case 'Queued': return 'queued';
    case 'Waiting': return 'waiting';
    case 'Fetching':
    case 'EvaluatingFlake':
    case 'EvaluatingDerivation':
    case 'Building': return 'running';
    case 'Completed': return 'success';
    case 'Failed': return 'failure';
    case 'Aborted': return 'aborted';
  }
}

export function isEvaluationNotStartedStatus(status: EvaluationStatus): boolean {
  return status === 'Queued' || status === 'Waiting';
}

export function abortedWithoutBuildsTitle(startedAt: string | null | undefined): string {
  return startedAt ? 'Aborted before any builds started' : 'Aborted before the evaluation started';
}

export function buildPhase(status: BuildStatus): StatusPhase {
  switch (status) {
    case 'Created':
    case 'Queued': return 'queued';
    case 'Building': return 'running';
    case 'Completed':
    case 'Substituted': return 'success';
    case 'FailedPermanent':
    case 'FailedTransient':
    case 'FailedTimeout': return 'failure';
    case 'Aborted':
    case 'DependencyFailed':
    case 'Skipped': return 'aborted';
  }
}

const BUILD_STATUS_LABEL: Record<BuildStatus, string> = {
  Created: 'Created',
  Queued: 'Queued',
  Building: 'Building',
  Completed: 'Completed',
  Substituted: 'Substituted',
  FailedPermanent: 'Failed',
  FailedTransient: 'Worker fault',
  FailedTimeout: 'Timed out',
  Aborted: 'Aborted',
  DependencyFailed: 'Skipped: Dependency failure',
  Skipped: 'Skipped',
};

export function buildOutcomeLabel(status: string): string {
  return BUILD_STATUS_LABEL[status as BuildStatus] ?? status;
}

export function inputFetchPhase(state: InputFetchState): StatusPhase {
  switch (state) {
    case 'Queued': return 'queued';
    case 'Fetching': return 'running';
    case 'Done': return 'success';
    case 'Failed': return 'failure';
  }
}

/// An entry point still waiting on its own build reads as running while any of its dependencies build.
export function entryPointPhase(ep: Pick<EntryPointSummary, 'build_status' | 'deps'>): StatusPhase {
  const phase = buildPhase(ep.build_status);
  return phase === 'queued' && ep.deps.building > 0 ? 'running' : phase;
}

export function execJobPhase(status: ExecJobStatus): StatusPhase {
  switch (status) {
    case 'created': return 'waiting';
    case 'queued': return 'queued';
    case 'starting':
    case 'running': return 'running';
    case 'completed': return 'success';
    case 'failed': return 'failure';
    case 'aborted': return 'aborted';
  }
}

export function entryPointPhaseWithExecJob(
  ep: Pick<EntryPointSummary, 'build_status' | 'deps'>,
  execJobStatus: ExecJobStatus | null | undefined,
): StatusPhase {
  const phase = entryPointPhase(ep);
  return phase === 'success' && execJobStatus ? execJobPhase(execJobStatus) : phase;
}

/// A finished evaluation stopped waiting for its unfinished work, even when another evaluation still builds the same package.
export function phaseInFinishedEvaluation(phase: StatusPhase, evaluationStatus: EvaluationStatus): StatusPhase {
  const unfinished = phase === 'queued' || phase === 'waiting' || phase === 'running';
  return unfinished && !isRunningEvaluationStatus(evaluationStatus) ? 'aborted' : phase;
}

const PENDING_BUILD_STATUSES: ReadonlySet<string> = new Set<BuildStatus>(['Created', 'Queued', 'Building']);

export function isPendingBuildStatus(status: string): boolean {
  return PENDING_BUILD_STATUSES.has(status);
}

const BUILD_STATUS_NAMES_BY_STATUS_CODE: readonly BuildStatus[] = [
  'Created', 'Queued', 'Building', 'Completed', 'FailedPermanent', 'Aborted',
  'DependencyFailed', 'Substituted', 'FailedTransient', 'FailedTimeout', 'Skipped',
];

export function buildStatusNameFromStatusCode(code: number | undefined): BuildStatus | undefined {
  return code === undefined ? undefined : BUILD_STATUS_NAMES_BY_STATUS_CODE[code];
}
