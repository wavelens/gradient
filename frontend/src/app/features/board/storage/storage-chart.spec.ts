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
    const { categories, series } = alignSeries(S, (p) => p.avg);
    expect(categories).toEqual(['2026-09-29T15:00:00+00:00', '2026-09-29T15:01:00+00:00']);
    expect(series).toEqual([
      { name: 'get', data: [5, 4] },
      { name: 'put', data: [null, 3] },
    ]);
  });

  it('is empty for no series', () => {
    expect(alignSeries([], (p) => p.avg)).toEqual({ categories: [], series: [] });
  });

  it('names an unlabelled series by the fallback', () => {
    const { series } = alignSeries([{ label: '', points: S[0].points }], (p) => p.max, 'value');
    expect(series[0].name).toBe('value');
  });
});
