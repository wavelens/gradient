/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildMetricChartOption } from './metric-chart.options';

const cats = ['10:00', '11:00', '12:00'];
const one = [{ name: 'build', data: [1, 2, 3] }];
const two = [
  { name: 'avg', data: [1, 2, 3] },
  { name: 'max', data: [4, 5, 6] },
];

const THEME = {
  text: '#abb0b4',
  grid: '#2d333b',
  border: '#2d333b',
  surface: '#21262d',
  palette: ['#3b82f6', '#ef4444', '#22c55e', '#f97316'],
};

describe('buildMetricChartOption cartesian types', () => {
  it('renders area as a smooth line series with a gradient fill', () => {
    const opt = buildMetricChartOption({ type: 'area', series: one, categories: cats }, THEME);
    const s = (opt.series as any[])[0];
    expect(s.type).toBe('line');
    expect(s.smooth).toBe(true);
    expect(s.areaStyle).toBeTruthy();
  });

  it('renders line without an area fill', () => {
    const opt = buildMetricChartOption({ type: 'line', series: one, categories: cats }, THEME);
    const s = (opt.series as any[])[0];
    expect(s.type).toBe('line');
    expect(s.areaStyle).toBeUndefined();
  });

  it('renders bar as a bar series with the category axis on x', () => {
    const opt = buildMetricChartOption({ type: 'bar', series: one, categories: cats }, THEME);
    expect((opt.series as any[])[0].type).toBe('bar');
    expect((opt.xAxis as any).type).toBe('category');
    expect((opt.xAxis as any).data).toEqual(cats);
    expect((opt.yAxis as any).type).toBe('value');
  });

  it('swaps the axes for a horizontal bar', () => {
    const opt = buildMetricChartOption({ type: 'bar', series: one, categories: cats, horizontal: true }, THEME);
    expect((opt.xAxis as any).type).toBe('value');
    expect((opt.yAxis as any).type).toBe('category');
    expect((opt.yAxis as any).data).toEqual(cats);
  });

  it('keeps null gaps in the data instead of coercing them to zero', () => {
    const opt = buildMetricChartOption({ type: 'line', series: [{ name: 'a', data: [1, null, 3] }], categories: cats }, THEME);
    expect((opt.series as any[])[0].data).toEqual([1, null, 3]);
  });
});

describe('buildMetricChartOption presentation', () => {
  it('passes colors through', () => {
    const opt = buildMetricChartOption({ type: 'line', series: one, categories: cats, colors: ['#abc123'] }, THEME);
    expect(opt.color).toEqual(['#abc123']);
  });

  it('shows a legend only when there is more than one series', () => {
    expect((buildMetricChartOption({ type: 'line', series: one, categories: cats }, THEME).legend as any).show).toBe(false);
    expect((buildMetricChartOption({ type: 'line', series: two, categories: cats }, THEME).legend as any).show).toBe(true);
  });

  it('applies valueFormatter to the value axis and the tooltip', () => {
    const opt = buildMetricChartOption({
      type: 'line',
      series: one,
      categories: cats,
      valueFormatter: (v) => `${v} MB`,
    }, THEME);
    expect((opt.yAxis as any).axisLabel.formatter(7)).toBe('7 MB');
    expect((opt.tooltip as any).valueFormatter(7)).toBe('7 MB');
  });

  it('sets the y axis title when given', () => {
    const opt = buildMetricChartOption({ type: 'line', series: one, categories: cats, yAxisTitle: 'seconds' }, THEME);
    expect((opt.yAxis as any).name).toBe('seconds');
  });

  it('uses the dark surface theme colors', () => {
    const opt = buildMetricChartOption({ type: 'line', series: one, categories: cats }, THEME);
    expect((opt.xAxis as any).axisLabel.color).toBe(THEME.text);
    expect(opt.backgroundColor).toBe('transparent');
  });
});

describe('buildMetricChartOption radar', () => {
  it('builds indicators from the categories and emits a radar series', () => {
    const opt = buildMetricChartOption({ type: 'radar', series: one, categories: cats }, THEME);
    expect((opt.radar as any).indicator).toEqual(cats.map((c) => ({ name: c })));
    const s = (opt.series as any[])[0];
    expect(s.type).toBe('radar');
    expect(s.data[0].value).toEqual([1, 2, 3]);
    expect(s.data[0].name).toBe('build');
    expect(opt.xAxis).toBeUndefined();
    expect(opt.yAxis).toBeUndefined();
  });
});

describe('buildMetricChartOption heatmap', () => {
  const bands = [
    { name: '0-10s', data: [{ x: '10:00', y: 5 }, { x: '11:00', y: 6 }] },
    { name: '10-60s', data: [{ x: '10:00', y: 1 }, { x: '11:00', y: 2 }] },
  ];

  it('derives the x categories from the first series points', () => {
    const opt = buildMetricChartOption({ type: 'heatmap', series: bands }, THEME);
    expect((opt.xAxis as any).data).toEqual(['10:00', '11:00']);
  });

  it('uses the series names as the y categories', () => {
    const opt = buildMetricChartOption({ type: 'heatmap', series: bands }, THEME);
    expect((opt.yAxis as any).data).toEqual(['0-10s', '10-60s']);
  });

  it('flattens xy points into [xIndex, yIndex, value] triples', () => {
    const opt = buildMetricChartOption({ type: 'heatmap', series: bands }, THEME);
    expect((opt.series as any[])[0].data).toEqual([
      [0, 0, 5],
      [1, 0, 6],
      [0, 1, 1],
      [1, 1, 2],
    ]);
  });

  it('scales the visual map to the largest value present', () => {
    const opt = buildMetricChartOption({ type: 'heatmap', series: bands }, THEME);
    expect((opt.visualMap as any).max).toBe(6);
  });

  it('survives an empty series list', () => {
    const opt = buildMetricChartOption({ type: 'heatmap', series: [] }, THEME);
    expect((opt.series as any[])[0].data).toEqual([]);
    expect((opt.visualMap as any).max).toBe(0);
  });
});

describe('buildMetricChartOption dual axis', () => {
  const dual = {
    type: 'area' as const,
    categories: cats,
    series: [
      { name: 'Bytes served', data: [10, 20, 30] },
      { name: 'Requests', data: [1, 2, 3], axis: 'right' as const },
    ],
    valueFormatter: (v: number) => `${v} B`,
    secondary: { title: 'Requests', valueFormatter: (v: number) => `${v} req` },
  };

  it('emits two value axes with the second on the right', () => {
    const y = buildMetricChartOption(dual, THEME).yAxis as any[];
    expect(y).toHaveLength(2);
    expect(y[0].position).toBeUndefined();
    expect(y[1].position).toBe('right');
    expect(y[1].name).toBe('Requests');
  });

  it('binds a right-axis series to the second axis', () => {
    const s = buildMetricChartOption(dual, THEME).series as any[];
    expect(s[0].yAxisIndex).toBe(0);
    expect(s[1].yAxisIndex).toBe(1);
  });

  it('formats each axis with its own formatter', () => {
    const y = buildMetricChartOption(dual, THEME).yAxis as any[];
    expect(y[0].axisLabel.formatter(5)).toBe('5 B');
    expect(y[1].axisLabel.formatter(5)).toBe('5 req');
  });

  it('formats each tooltip row with the formatter of its own axis', () => {
    const tooltip = buildMetricChartOption(dual, THEME).tooltip as any;
    const text = tooltip.formatter([
      { axisValueLabel: '10:00', marker: 'M0', seriesName: 'Bytes served', seriesIndex: 0, value: 10 },
      { axisValueLabel: '10:00', marker: 'M1', seriesName: 'Requests', seriesIndex: 1, value: 1 },
    ]);
    expect(text).toContain('10 B');
    expect(text).toContain('1 req');
    expect(text).toContain('10:00');
  });

  it('keeps a single axis when no secondary is configured', () => {
    const opt = buildMetricChartOption({ type: 'area', series: one, categories: cats }, THEME);
    expect(Array.isArray(opt.yAxis)).toBe(false);
    expect((opt.series as any[])[0].yAxisIndex).toBeUndefined();
  });
});
