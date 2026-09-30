/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BoardStorage, LabelledPoint, LabelledSeries } from '@core/services/board.service';
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

const HOUR_MS = 3_600_000;
const STEP_MS: Record<BoardStorage['granularity'], number> = {
  minute: 60_000,
  hour: HOUR_MS,
  day: 24 * HOUR_MS,
};

/// Every bucket start of the window the server answered, so all charts share one time axis.
export function windowBuckets(granularity: BoardStorage['granularity'], hours: number, now: number): number[] {
  const step = STEP_MS[granularity];
  const first = Math.ceil((now - hours * HOUR_MS) / step) * step;
  const last = Math.floor(now / step) * step;

  return Array.from({ length: Math.max(0, (last - first) / step + 1) }, (_, i) => first + i * step);
}

export function alignSeries(buckets: number[], ...groups: SeriesGroup[]): MetricSeries[] {
  return groups.flatMap((g) => g.series.map((s) => alignOne(g, s, buckets)));
}

function alignOne(group: SeriesGroup, input: LabelledSeries, buckets: number[]): MetricSeries {
  const byBucket = new Map(input.points.map((p) => [Date.parse(p.bucket_start), group.pick(p)]));
  const missing = group.counts ? 0 : null;
  const label = input.label || 'total';

  return {
    name: group.name ? group.name(label) : label,
    data: buckets.map((b) => byBucket.get(b) ?? missing),
    ...(group.axis ? { axis: group.axis } : {}),
    ...(group.type ? { type: group.type } : {}),
  };
}
