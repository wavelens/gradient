/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { LabelledPoint, LabelledSeries } from '@core/services/board.service';
import type { MetricAxis, MetricSeries, MetricSeriesType } from '@shared/ui';

export interface SeriesGroup {
  series: LabelledSeries[];
  pick: (p: LabelledPoint) => number;
  name?: (label: string) => string;
  axis?: MetricAxis;
  type?: MetricSeriesType;
  /// A missing row means zero events for a count, but no sample for a gauge or latency.
  counts?: boolean;
}

export type Aligned = { categories: string[]; series: MetricSeries[] };

export function alignSeries(...groups: SeriesGroup[]): Aligned {
  const categories = [
    ...new Set(groups.flatMap((g) => g.series.flatMap((s) => s.points.map((p) => p.bucket_start)))),
  ].sort();
  const series = groups.flatMap((g) => g.series.map((s) => alignOne(g, s, categories)));

  return { categories, series };
}

function alignOne(group: SeriesGroup, input: LabelledSeries, categories: string[]): MetricSeries {
  const byBucket = new Map(input.points.map((p) => [p.bucket_start, group.pick(p)]));
  const missing = group.counts ? 0 : null;
  const label = input.label || 'total';

  return {
    name: group.name ? group.name(label) : label,
    data: categories.map((c) => byBucket.get(c) ?? missing),
    ...(group.axis ? { axis: group.axis } : {}),
    ...(group.type ? { type: group.type } : {}),
  };
}
