/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus } from '@core/models';
import type { EvaluationFailureSummary, FailedBuildSummary } from '@core/services/evaluations.service';
import type { BadgeSeverity } from '@gradient/ui/ui';

const BLOCKED_NAMED = 5;

function counted(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? '' : 's'}`;
}

export function headline(summary: EvaluationFailureSummary, running: boolean): string {
  const { total, built, failed, blocked, unfinished } = summary.packages;
  const broken = failed + blocked + summary.failed_attributes.length;
  if (running) return `Still running, ${built} of ${counted(total, 'package')} built so far`;
  if (broken > 0) return `${counted(broken, 'package')} did not build`;
  if (unfinished > 0) return `${counted(unfinished, 'package')} not built`;

  return `All ${counted(total, 'package')} built`;
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

  return `Blocks ${counted(failure.blocked_total, 'package')}: ${names}`;
}

export function firstLine(message: string): string {
  return message.split('\n').map((line) => line.trim()).find((line) => line.length > 0) ?? '';
}
