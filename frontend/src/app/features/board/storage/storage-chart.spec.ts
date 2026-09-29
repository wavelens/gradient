import { describe, expect, it } from 'vitest';
import type { LabelledSeries } from '@core/services/board.service';
import { alignSeries } from './storage-chart';

const S: LabelledSeries[] = [
  {
    label: 'get',
    points: [
      { bucket_start: '2026-09-29T15:00:00+00:00', count: 2, avg: 5, max: 8 },
      { bucket_start: '2026-09-29T15:01:00+00:00', count: 1, avg: 4, max: 4 },
    ],
  },
  { label: 'put', points: [{ bucket_start: '2026-09-29T15:01:00+00:00', count: 1, avg: 3, max: 3 }] },
];

describe('alignSeries', () => {
  it('shares one sorted category axis and fills gaps with null', () => {
    const { categories, series } = alignSeries({ series: S, pick: (p) => p.avg });
    expect(categories).toEqual(['2026-09-29T15:00:00+00:00', '2026-09-29T15:01:00+00:00']);
    expect(series).toEqual([
      { name: 'get', data: [5, 4] },
      { name: 'put', data: [null, 3] },
    ]);
  });

  it('fills missing buckets of a count series with zero', () => {
    const { series } = alignSeries({ series: S, pick: (p) => p.count, counts: true });
    expect(series[1].data).toEqual([0, 1]);
  });

  it('is empty for no series', () => {
    expect(alignSeries({ series: [], pick: (p) => p.avg })).toEqual({ categories: [], series: [] });
  });

  it('names an unlabelled series total', () => {
    const { series } = alignSeries({ series: [{ label: '', points: S[0].points }], pick: (p) => p.max });
    expect(series[0].name).toBe('total');
  });

  it('aligns every group on one axis and carries axis, type and name per group', () => {
    const stalls: LabelledSeries[] = [
      { label: 'bulk', points: [{ bucket_start: '2026-09-29T15:02:00+00:00', count: 3, avg: 3, max: 3 }] },
    ];
    const { categories, series } = alignSeries(
      { series: S, pick: (p) => p.avg, type: 'line' },
      { series: stalls, pick: (p) => p.count, name: (l) => `${l} stalls`, axis: 'right', type: 'bar', counts: true }
    );
    expect(categories).toHaveLength(3);
    expect(series).toEqual([
      { name: 'get', data: [5, 4, null], type: 'line' },
      { name: 'put', data: [null, 3, null], type: 'line' },
      { name: 'bulk stalls', data: [0, 0, 3], axis: 'right', type: 'bar' },
    ]);
  });
});
