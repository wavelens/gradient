import { describe, expect, it } from 'vitest';
import type { LabelledSeries } from '@core/services/board.service';
import { alignSeries, windowBuckets } from './storage-chart';

const T0 = Date.parse('2026-09-29T15:00:00Z');
const MIN = 60_000;
const BUCKETS = [T0, T0 + MIN, T0 + 2 * MIN];

const S: LabelledSeries[] = [
  {
    label: 'get',
    points: [
      { bucket_start: '2026-09-29T15:00:00+00:00', count: 2, avg: 5, max: 8 },
      { bucket_start: '2026-09-29T15:01:00+00:00', count: 1, avg: 4, max: 4 },
    ],
  },
  { label: 'put', points: [{ bucket_start: '2026-09-29T15:02:00+00:00', count: 1, avg: 3, max: 3 }] },
];

describe('windowBuckets', () => {
  it('lists every bucket the window covers, including ones without samples', () => {
    const now = T0 + 60 * MIN + 30_000;
    const buckets = windowBuckets('minute', 1, now);
    expect(buckets).toHaveLength(60);
    expect(buckets[0]).toBe(T0 + MIN);
    expect(buckets.at(-1)).toBe(T0 + 60 * MIN);
  });

  it('steps by the granularity', () => {
    const buckets = windowBuckets('hour', 24, T0);
    expect(buckets).toHaveLength(25);
    expect(buckets[1] - buckets[0]).toBe(60 * MIN);
  });
});

describe('alignSeries', () => {
  it('places each point on its bucket and leaves gauge gaps null', () => {
    expect(alignSeries(BUCKETS, { series: S, pick: (p) => p.avg })).toEqual([
      { name: 'get', data: [5, 4, null] },
      { name: 'put', data: [null, null, 3] },
    ]);
  });

  it('fills quiet buckets of a count series with zero', () => {
    const series = alignSeries(BUCKETS, { series: S, pick: (p) => p.count, counts: true });
    expect(series[1].data).toEqual([0, 0, 1]);
  });

  it('names an unlabelled series total', () => {
    const series = alignSeries(BUCKETS, { series: [{ label: '', points: S[0].points }], pick: (p) => p.max });
    expect(series[0].name).toBe('total');
  });

  it('carries axis, type and name per group', () => {
    const series = alignSeries(
      BUCKETS,
      { series: [S[0]], pick: (p) => p.avg, type: 'line' },
      { series: [S[1]], pick: (p) => p.count, name: (l) => `${l} stalls`, axis: 'right', type: 'bar', counts: true }
    );
    expect(series).toEqual([
      { name: 'get', data: [5, 4, null], type: 'line' },
      { name: 'put stalls', data: [0, 0, 1], axis: 'right', type: 'bar' },
    ]);
  });
});
