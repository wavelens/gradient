/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildStatusCounts } from '@core/models';

export interface BuildTally {
  done: number;
  total: number;
  overall: number;
  failed: number;
}

/** The numbers next to the bar match the fill and count the builds that gr-segmented-bar can draw. Aborted builds and builds with a failed dependency count as done. Substituted builds count only toward the overall total. */
export function buildTally(c: BuildStatusCounts): BuildTally {
  const done = c.completed + c.failed + c.aborted;
  const total = done + c.building + c.queued;
  const overall = total + c.substituted;
  const failed = c.failed;
  return total === 0 ? { done: c.substituted, total: c.substituted, overall, failed } : { done, total, overall, failed };
}
