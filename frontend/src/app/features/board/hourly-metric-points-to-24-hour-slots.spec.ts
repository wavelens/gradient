/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { metricPointsTo24HourChartSeries } from './hourly-metric-points-to-24-hour-slots';

const NOW = Date.parse('2026-11-09T12:30:00Z');
const point = (hour: string, count: number) => ({ bucket_start: `2026-11-09T${hour}:00:00+00:00`, count });

describe('metricPointsTo24HourChartSeries', () => {
  it('lays out 24 hourly slots and keeps empty hours apart: zero for counts, a gap for gauges, and leaves out the running hour', () => {
    const points = [point('08', 3), point('11', 5), point('12', 7)];
    const { categories, series } = metricPointsTo24HourChartSeries(
      [
        { name: 'count', points, pick: (p) => p.count, missing: 0 },
        { name: 'gauge', points, pick: (p) => p.count, missing: null },
      ],
      NOW,
    );
    expect(categories).toHaveLength(24);
    expect(series[0].data.slice(-4)).toEqual([3, 0, 0, 5]);
    expect(series[1].data.slice(-4)).toEqual([3, null, null, 5]);
  });

  it('drops hours older than the window', () => {
    const { series } = metricPointsTo24HourChartSeries(
      [{ name: 'count', points: [point('11', 1), { bucket_start: '2026-11-08T11:00:00Z', count: 9 }], pick: (p) => p.count, missing: 0 }],
      NOW,
    );
    expect((series[0].data as number[]).reduce((a, b) => a + b, 0)).toBe(1);
  });

  it('returns no slots without any point, so the chart shows its empty state', () => {
    expect(metricPointsTo24HourChartSeries([{ name: 'count', points: [], pick: () => 0, missing: 0 }], NOW).categories).toEqual([]);
  });
});
