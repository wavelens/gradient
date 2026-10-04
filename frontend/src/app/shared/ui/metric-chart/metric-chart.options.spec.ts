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
  textStrong: '#ffffff',
  muted: '#8b949e',
  mono: "'Space Mono', monospace",
  grid: '#2d333b',
  border: '#2d333b',
  surface: '#21262d',
  palette: ['#3b82f6', '#ef4444', '#22c55e', '#f97316'],
};

describe('chart tooltip', () => {
  it('is themed rather than left on the ECharts light default', () => {
    const opt = buildMetricChartOption(
      { type: 'line', series: [{ name: 'a', data: [1, 2] }], categories: ['x', 'y'] },
      THEME,
    ) as Record<string, any>;
    expect(opt['tooltip'].backgroundColor).toBe(THEME.surface);
    expect(opt['tooltip'].borderColor).toBe(THEME.border);
    expect(opt['tooltip'].textStyle.color).toBe(THEME.textStrong);
  });

  it('themes the legend chrome, not just its labels', () => {
    const opt = buildMetricChartOption(
      { type: 'line', series: [{ name: 'a', data: [1] }, { name: 'b', data: [2] }], categories: ['x'] },
      THEME,
    ) as Record<string, any>;
    expect(opt['legend'].textStyle.color).toBe(THEME.text);
    expect(opt['legend'].inactiveColor).toBe(THEME.muted);
    expect(opt['legend'].pageIconColor).toBe(THEME.text);
  });
});

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

  it('caps the value axis at a fixed maximum when given', () => {
    const opt = buildMetricChartOption({ type: 'line', series: one, categories: cats, yAxisMax: 100 }, THEME);
    expect((opt.yAxis as any).max).toBe(100);
  });

  it('caps only the primary axis of a dual-axis chart', () => {
    const opt = buildMetricChartOption({
      type: 'line',
      series: [{ name: 'fill', data: [1] }, { name: 'stalls', data: [2], axis: 'right' }],
      categories: ['x'],
      yAxisMax: 100,
      secondary: { title: 'stalls' },
    }, THEME);
    const [left, right] = opt.yAxis as any[];
    expect(left.max).toBe(100);
    expect(right.max).toBeUndefined();
  });

  it('lets the value axis scale with the data by default', () => {
    const opt = buildMetricChartOption({ type: 'line', series: one, categories: cats }, THEME);
    expect((opt.yAxis as any).max).toBeUndefined();
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

  it('draws one set of grid lines across both axes', () => {
    const y = buildMetricChartOption(dual, THEME).yAxis as any[];
    expect(y[0].splitLine.show).not.toBe(false);
    expect(y[1].splitLine.show).toBe(false);
  });

  it('ticks both axes the same number of times so their grid lines coincide', () => {
    const y = buildMetricChartOption(dual, THEME).yAxis as any[];
    expect(y[0].splitNumber).toBe(y[1].splitNumber);
    expect(y[0].splitNumber).toBeGreaterThan(0);
  });

  it('renders a series typed bar as bars inside a line chart', () => {
    const opt = buildMetricChartOption(
      {
        type: 'line',
        categories: cats,
        series: [
          { name: 'fill', data: [1, 2, 3] },
          { name: 'stalls', data: [0, 1, 0], axis: 'right' as const, type: 'bar' as const },
        ],
        secondary: { title: 'stalls' },
      },
      THEME
    );
    const s = opt.series as any[];
    expect(s.map((x) => x.type)).toEqual(['line', 'bar']);
    expect(s[1].yAxisIndex).toBe(1);
    expect((opt.xAxis as any).boundaryGap).toBe(true);
  });

  it('marks the points of a gapped line typed explicitly', () => {
    const opt = buildMetricChartOption(
      {
        type: 'line',
        categories: cats,
        series: [
          { name: 'sparse', data: [null, 4, null], type: 'line' as const },
          { name: 'untyped', data: [null, 4, null] },
        ],
      },
      THEME
    );
    const s = opt.series as any[];
    expect(s[0].showSymbol).toBe(true);
    expect(s[1].showSymbol).toBe(false);
    expect((opt.xAxis as any).boundaryGap).toBe(false);
  });

  it('keeps a single axis when no secondary is configured', () => {
    const opt = buildMetricChartOption({ type: 'area', series: one, categories: cats }, THEME);
    expect(Array.isArray(opt.yAxis)).toBe(false);
    expect((opt.series as any[])[0].yAxisIndex).toBeUndefined();
  });
});

describe('plot inset', () => {
  it('pins the plot area and bucket slots so stacked charts line up', () => {
    const inset = { left: 64, right: 56 };
    const line = buildMetricChartOption({ type: 'line', series: one, categories: cats, inset }, THEME) as any;
    const bars = buildMetricChartOption(
      { type: 'bar', series: one, categories: cats, inset, secondary: { title: 'stalls' } },
      THEME
    ) as any;

    for (const opt of [line, bars]) {
      expect(opt.grid).toMatchObject({ left: 64, right: 56, containLabel: false });
      expect(opt.xAxis.boundaryGap).toBe(true);
    }
  });
});

describe('value axis minimum', () => {
  const axisMin = (axis: any, min: number, max: number) => axis.min({ min, max });
  const dual = {
    type: 'bar' as const,
    categories: cats,
    series: [
      { name: 'bytes', data: [0, 0, 0] },
      { name: 'errors', data: [0, 0, 0], axis: 'right' as const },
    ],
    secondary: { title: 'errors' },
  };

  it('starts an all-zero series at 0', () => {
    const y = buildMetricChartOption({ type: 'bar', series: [{ name: 'a', data: [0, 0, 0] }], categories: cats }, THEME).yAxis;
    expect(axisMin(y, 0, 0)).toBe(0);
  });

  it('starts positive data at 0 rather than at its smallest value', () => {
    const y = buildMetricChartOption({ type: 'line', series: one, categories: cats }, THEME).yAxis;
    expect(axisMin(y, 1, 3)).toBe(0);
  });

  it('still shows a real negative value', () => {
    const y = buildMetricChartOption({ type: 'bar', series: [{ name: 'a', data: [-8, 12] }], categories: cats }, THEME).yAxis;
    expect(axisMin(y, -8, 12)).toBe(-8);
  });

  it('applies to both axes of a dual-axis chart', () => {
    const [left, right] = buildMetricChartOption(dual, THEME).yAxis as any[];
    expect([axisMin(left, 0, 0), axisMin(right, 0, 0)]).toEqual([0, 0]);
    expect(axisMin(right, -2, 4)).toBe(-2);
  });

  it('applies to the value axis of a horizontal bar', () => {
    const x = buildMetricChartOption({ type: 'bar', series: one, categories: cats, horizontal: true }, THEME).xAxis;
    expect(axisMin(x, 0, 0)).toBe(0);
  });
});
