/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EvaluationFailureSummary, EvaluationStatus, FailedBuildSummary } from '@core/models';
import type { BadgeSeverity } from '@gradient/ui/ui';
import { buildStatusLabel, isRunningEvaluationStatus } from '@shared/evaluation';
import { plural } from '@shared/text';

const BLOCKED_NAMED = 5;

export interface SummaryCountTile {
  label: string;
  value: number;
}

export function getEvaluationSummaryCountTiles(summary: EvaluationFailureSummary, running: boolean): SummaryCountTile[] {
  const { built, failed, blocked, unfinished } = summary.packages;
  return [
    { label: 'Built', value: built },
    { label: 'Failed', value: failed },
    ...(blocked > 0 ? [{ label: buildStatusLabel('DependencyFailed'), value: blocked }] : []),
    ...(unfinished > 0 && !running ? [{ label: 'Not built', value: unfinished }] : []),
    { label: 'Failed to evaluate', value: summary.failed_attributes.length },
  ];
}

export function headline(summary: EvaluationFailureSummary, status: EvaluationStatus): string {
  const { total, built, failed, blocked, unfinished } = summary.packages;
  if (status === 'Queued' || status === 'Waiting') return 'Not started yet';
  if (isRunningEvaluationStatus(status)) {
    return total ? `Still running, ${built} of ${plural(total, 'package')} built so far` : 'Still evaluating';
  }
  const failedAttributes = summary.failed_attributes.length;
  const problems = [
    ...(failed + blocked > 0 ? [`${plural(failed + blocked, 'package')} did not build`] : []),
    ...(failedAttributes > 0 ? [`${plural(failedAttributes, 'attribute')} failed to evaluate`] : []),
  ];
  const notBuilt = unfinished > 0 ? [`${plural(unfinished, 'package')} not built`] : [];
  if (status === 'Aborted') return ['Aborted', ...problems, ...notBuilt].join(', ');
  if (problems.length) return problems.join(', ');
  if (status === 'Failed') return 'Evaluation failed';
  if (notBuilt.length) return notBuilt[0];

  return `All ${plural(total, 'package')} built`;
}

// A timeout or a lost worker is a fault of the CI, not of the package.
export function failureKind(status: BuildStatus): { label: string; severity: BadgeSeverity } {
  if (status === 'FailedTimeout') return { label: 'Timed out', severity: 'warning' };
  if (status === 'FailedTransient') return { label: 'Worker fault', severity: 'warning' };

  return { label: 'Build failed', severity: 'danger' };
}

export function failureTitle(failure: FailedBuildSummary): string {
  return failure.attributes.length ? failure.attributes.join(', ') : failure.name;
}

export function blockedLine(failure: FailedBuildSummary): string {
  if (!failure.blocked_total) return '';
  const named = failure.blocked.slice(0, BLOCKED_NAMED);
  const unnamed = failure.blocked_total - named.length;
  const names = named.join(', ') + (unnamed > 0 ? ` and ${unnamed} more` : '');

  return `Blocks ${plural(failure.blocked_total, 'package')}: ${names}`;
}

export function firstLine(message: string): string {
  return message.split('\n').map((line) => line.trim()).find((line) => line.length > 0) ?? '';
}
