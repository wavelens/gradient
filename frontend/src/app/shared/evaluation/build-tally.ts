/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatusCounts, EvaluationStatus } from '@core/models';
import { countBuildStatusesAfterEvaluationFinished } from './evaluation-build-view';

export interface BuildTally {
  done: number;
  total: number;
  overall: number;
  failed: number;
  notBuilt: number;
  aborted: number;
  counts: BuildStatusCounts;
}

/** The numbers next to the bar match the fill and count the builds that gr-segmented-bar can draw. Canceled builds and builds that a dependency failure left unbuilt count as complete. Substitute builds add to the overall total only. */
export function buildTally(counts: BuildStatusCounts, evaluationStatus: EvaluationStatus): BuildTally {
  const c = countBuildStatusesAfterEvaluationFinished(counts, evaluationStatus);
  const done = c.completed + c.failed + c.aborted + c.dependency_failed;
  const total = done + c.building + c.queued;
  const overall = total + c.substituted;
  const counted = { overall, failed: c.failed, notBuilt: c.dependency_failed, aborted: c.aborted, counts: c };
  return total === 0 ? { done: c.substituted, total: c.substituted, ...counted } : { done, total, ...counted };
}
