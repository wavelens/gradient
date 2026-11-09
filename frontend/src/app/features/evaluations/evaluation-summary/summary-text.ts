/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EvaluationFailureSummary, EvaluationStatus, FailedBuildSummary } from '@core/models';
import type { BadgeSeverity } from '@gradient/ui/ui';
import { buildStatusLabel, isEvaluationNotStartedStatus, isRunningEvaluationStatus } from '@shared/evaluation';
import { removeAnsiEscapeSequences } from '@shared/log';
import { plural } from '@shared/text';

const BLOCKED_NAMED = 5;

export interface SummaryCountTile {
  label: string;
  value: number;
  isValueZero: boolean;
}

function countTile(value: number, singular: string, pluralLabel: string): SummaryCountTile {
  return { label: value === 1 ? singular : pluralLabel, value, isValueZero: value === 0 };
}

export function getEvaluationSummaryCountTiles(summary: EvaluationFailureSummary, status: EvaluationStatus): SummaryCountTile[] {
  if (isEvaluationNotStartedStatus(status)) return [];
  const { failed, blocked, built, unfinished } = summary.packages;
  return [
    countTile(failed, 'Failed package', 'Failed packages'),
    countTile(blocked, 'Blocked package', 'Blocked packages'),
    countTile(built, 'Built package', 'Built packages'),
    isRunningEvaluationStatus(status)
      ? countTile(unfinished, 'Remaining package', 'Remaining packages')
      : countTile(unfinished, 'Unbuilt package', 'Unbuilt packages'),
  ];
}

export function headline(summary: EvaluationFailureSummary, status: EvaluationStatus): string {
  const { total, built, failed, blocked, unfinished } = summary.packages;
  if (isEvaluationNotStartedStatus(status)) return 'Not started yet';
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
  if (status === 'FailedTimeout' || status === 'FailedTransient') return { label: buildStatusLabel(status), severity: 'warning' };

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
  return removeAnsiEscapeSequences(message).split('\n').map((line) => line.trim()).find((line) => line.length > 0) ?? '';
}
