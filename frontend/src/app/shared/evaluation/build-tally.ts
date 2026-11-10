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
  notBuilt: number;
  aborted: number;
}

/** The numbers next to the bar match the fill and count the builds that gr-segmented-bar can draw. Canceled builds and builds that a dependency failure left unbuilt count as complete. Substitute builds add to the overall total only. */
export function buildTally(c: BuildStatusCounts): BuildTally {
  const done = c.completed + c.failed + c.aborted + c.dependency_failed;
  const total = done + c.building + c.queued;
  const overall = total + c.substituted;
  const counted = { overall, failed: c.failed, notBuilt: c.dependency_failed, aborted: c.aborted };
  return total === 0 ? { done: c.substituted, total: c.substituted, ...counted } : { done, total, ...counted };
}
