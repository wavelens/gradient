/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BarSegment } from './segmented-bar.component';

export function ratioSegments(done: number, total: number | null): BarSegment[] {
  if (!total) return [{ tone: 'building', pct: 100 }];
  const pct = Math.min(1, done / total) * 100;
  return [{ tone: 'building', pct }, { tone: 'queued', pct: 100 - pct }];
}
