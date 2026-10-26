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
}

/** Counts the builds gr-segmented-bar draws, so the numbers next to a bar match its fill. */
export function buildTally(c: BuildStatusCounts): BuildTally {
  const done = c.completed + c.failed;
  const total = done + c.building + c.queued;
  const overall = total + c.substituted + c.aborted;
  return total === 0 ? { done: c.substituted, total: c.substituted, overall } : { done, total, overall };
}
