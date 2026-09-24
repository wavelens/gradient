/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EntryPointSummary, EvaluationStatus } from '@core/models';

export type StatusPhase = 'queued' | 'waiting' | 'running' | 'success' | 'failure' | 'aborted';

export function evaluationPhase(status: EvaluationStatus): StatusPhase {
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

/// An entry point still waiting on its own build reads as running while any of its dependencies build.
export function entryPointPhase(ep: Pick<EntryPointSummary, 'build_status' | 'deps'>): StatusPhase {
  const phase = buildPhase(ep.build_status);
  return phase === 'queued' && ep.deps.building > 0 ? 'running' : phase;
}

const PENDING_BUILD_STATUSES: ReadonlySet<string> = new Set<BuildStatus>(['Created', 'Queued', 'Building']);

export function isPendingBuildStatus(status: string): boolean {
  return PENDING_BUILD_STATUSES.has(status);
}
