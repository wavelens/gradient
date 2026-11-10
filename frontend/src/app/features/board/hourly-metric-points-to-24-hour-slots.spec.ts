/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { metricPointsTo24HourChartSeries } from './hourly-metric-points-to-24-hour-slots';

const NOW = Date.parse('2026-11-09T12:30:00Z');
const point = (hour: string, count: number) => ({ bucket_start: `2026-11-09T${hour}:00:00+00:00`, count });

describe('metricPointsTo24HourChartSeries', () => {
  it('lays out 24 hourly slots ending with the running hour, zero for empty counts and a gap for empty gauges', () => {
    const points = [point('08', 3), point('11', 5), point('12', 7)];
    const { categories, series } = metricPointsTo24HourChartSeries(
      [
        { name: 'count', points, pick: (p) => p.count, missing: 0 },
        { name: 'gauge', points, pick: (p) => p.count, missing: null },
      ],
      NOW,
    );
    expect(categories).toHaveLength(24);
    expect(series[0].data.slice(-5)).toEqual([3, 0, 0, 5, 7]);
    expect(series[1].data.slice(-5)).toEqual([3, null, null, 5, 7]);
  });

  it('keeps 24 slots ending with the new hour right at the hour boundary', () => {
    const { series } = metricPointsTo24HourChartSeries(
      [{ name: 'count', points: [point('12', 7), { bucket_start: '2026-11-08T12:00:00Z', count: 9 }], pick: (p) => p.count, missing: 0 }],
      Date.parse('2026-11-09T12:00:00Z'),
    );
    expect(series[0].data).toHaveLength(24);
    expect(series[0].data.at(-1)).toBe(7);
    expect(series[0].data[0]).toBe(0);
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
