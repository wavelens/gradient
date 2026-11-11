/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatusCounts, EvaluationStatus } from '@core/models';
import { plural } from '@shared/text';
import { countBuildStatusesAfterEvaluationFinished } from './evaluation-build-view';

export interface BuildCounts {
  built: number;
  total: number;
  failed: number;
  skipped: number;
  aborted: number;
}

export interface BuildTally extends BuildCounts {
  overall: number;
  counts: BuildStatusCounts;
}

export interface CountLineTextPiece {
  text: string;
  tone: 'built' | 'failed' | 'skipped' | 'aborted';
}

/** One count line for the task page, the evaluation log and the summary. The single denominator includes every build that this evaluation needed to build. Substituted builds count only when nothing else needed a build, so the numbers match the bar. */
export function buildTally(counts: BuildStatusCounts, evaluationStatus: EvaluationStatus): BuildTally {
  const c = countBuildStatusesAfterEvaluationFinished(counts, evaluationStatus);
  const total = c.completed + c.failed + c.aborted + c.dependency_failed + c.building + c.queued;
  const settled = { failed: c.failed, skipped: c.dependency_failed, aborted: c.aborted, overall: total + c.substituted, counts: c };
  return total === 0 ? { built: c.substituted, total: c.substituted, ...settled } : { built: c.completed, total, ...settled };
}

export function getBuildCountTextPieces(counts: BuildCounts, unit?: string): CountLineTextPiece[] {
  const settled: [number, CountLineTextPiece['tone']][] = [[counts.failed, 'failed'], [counts.skipped, 'skipped'], [counts.aborted, 'aborted']];
  return [
    { text: `${counts.built} of ${unit ? plural(counts.total, unit) : counts.total} built`, tone: 'built' },
    ...settled.filter(([count]) => count > 0).map(([count, tone]) => ({ text: `${count} ${tone}`, tone })),
  ];
}
