/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { LabelledPoint, LabelledSeries } from '@core/services/board.service';
import type { MetricSeries } from '@shared/ui';

export function alignSeries(
  input: LabelledSeries[],
  pick: (p: LabelledPoint) => number,
  fallback = 'total'
): { categories: string[]; series: MetricSeries[] } {
  const categories = [...new Set(input.flatMap((s) => s.points.map((p) => p.bucket_start)))].sort();
  const series = input.map((s) => {
    const byBucket = new Map(s.points.map((p) => [p.bucket_start, pick(p)]));
    return { name: s.label || fallback, data: categories.map((c) => byBucket.get(c) ?? null) };
  });

  return { categories, series };
}
