/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EChartsOption } from 'echarts';
import { bytesPerFormatterInput } from '@shared/text';

export type MetricChartType = 'area' | 'bar' | 'line' | 'radar' | 'heatmap';

export type MetricPointXY = { x: string; y: number };
export type MetricAxis = 'left' | 'right';
export type MetricSeriesType = 'bar' | 'line';
export type MetricSeries = {
  name: string;
  data: (number | null)[] | MetricPointXY[];
  axis?: MetricAxis;
  type?: MetricSeriesType;
  isLineDashed?: boolean;
};

export interface MetricChartConfig {
  type: MetricChartType;
  series: MetricSeries[];
  categories?: string[];
  colors?: string[];
  horizontal?: boolean;
  yAxisTitle?: string;
  yAxisMax?: number;
  valueFormatter?: (value: number) => string;
  secondary?: { title?: string; valueFormatter?: (value: number) => string };
  /// Fixed plot margins in px: charts stacked with the same inset share their x positions.
  inset?: { left: number; right: number };
  /// Narrow charts omit the title of the value axis and show fewer ticks on that axis. Put the legend below the plot.
  compact?: boolean;
}

/// Resolved from semantic tokens by the component, so charts follow the active theme.
export interface ChartTheme {
  text: string;
  textStrong: string;
  muted: string;
  mono: string;
  grid: string;
  border: string;
  surface: string;
  palette: string[];
}

const isXY = (d: MetricSeries['data']): d is MetricPointXY[] =>
  d.length > 0 && typeof d[0] === 'object' && d[0] !== null;

/// ECharts defaults to a white tooltip, so it has to be themed explicitly.
const tooltipChrome = (t: ChartTheme) => ({
  backgroundColor: t.surface,
  borderColor: t.border,
  borderWidth: 1,
  textStyle: { color: t.textStrong },
  axisPointer: {
    lineStyle: { color: t.border },
    crossStyle: { color: t.border },
    label: { backgroundColor: t.surface, borderColor: t.border, color: t.textStrong },
  },
});

const valuesOf = (s: MetricSeries) => (isXY(s.data) ? s.data.map((p) => p.y) : s.data) as (number | null)[];

/// Whole-number data gets whole-number ticks; otherwise a formatter that rounds prints "0 B 0 B 1 B".
const areAllSeriesValuesWholeNumbers = (series: MetricSeries[]) =>
  series.every((s) => valuesOf(s).every((v) => v === null || Number.isInteger(v)));

const hasIsolatedNonNullValue = (data: (number | null)[]) =>
  data.some((v, i) => v !== null && (data[i - 1] ?? null) === null && (data[i + 1] ?? null) === null);

const axisLabel = (t: ChartTheme) => ({ color: t.text, fontSize: 12, fontFamily: t.mono });
const axisLine = (t: ChartTheme) => ({ lineStyle: { color: t.border } });
const AXIS_TICKS = 4;
const COMPACT_AXIS_TICKS = 2;
const MAX_CATEGORY_AXIS_LABEL_WIDTH_PIXELS = 120;
const splitLine = (t: ChartTheme) => ({ lineStyle: { color: t.grid, type: 'dashed' as const } });

/// Builds the full ECharts option for `gr-metric-chart`. Kept pure and free of
/// Angular/DOM so every chart shape is unit-testable without a renderer.
export function buildMetricChartOption(requested: MetricChartConfig, theme: ChartTheme): EChartsOption {
  const cfg = convertOneOrTwoCategoryRadarChartToBarChart(requested);
  const format = cfg.valueFormatter ?? ((v: number) => String(v));
  const base: EChartsOption = {
    backgroundColor: 'transparent',
    color: cfg.colors?.length ? [...cfg.colors] : theme.palette,
    legend: {
      show: cfg.series.length > 1,
      textStyle: { color: theme.text },
      // Defaults for the dimmed and paging chrome are light-theme greys.
      inactiveColor: theme.muted,
      pageTextStyle: { color: theme.text },
      pageIconColor: theme.text,
      pageIconInactiveColor: theme.muted,
      ...(cfg.compact ? { bottom: 0 } : { top: 0 }),
    },
    tooltip: {
      ...tooltipChrome(theme),
      trigger: cfg.type === 'heatmap' || cfg.type === 'radar' ? 'item' : 'axis',
      valueFormatter: (v) => format(Number(v)),
    },
  };

  if (cfg.type === 'radar') return { ...base, ...radarOption(cfg, theme) };
  if (cfg.type === 'heatmap') return { ...base, ...heatmapOption(cfg, format, theme) };
  return { ...base, ...cartesianOption(cfg, format, theme) };
}

const RADAR_CHART_MIN_CATEGORY_COUNT = 3;

function convertOneOrTwoCategoryRadarChartToBarChart(cfg: MetricChartConfig): MetricChartConfig {
  const count = cfg.categories?.length ?? 0;
  return cfg.type === 'radar' && count > 0 && count < RADAR_CHART_MIN_CATEGORY_COUNT ? { ...cfg, type: 'bar' } : cfg;
}

function cartesianOption(cfg: MetricChartConfig, format: (v: number) => string, theme: ChartTheme): EChartsOption {
  const kindOf = (s: MetricSeries): MetricSeriesType => s.type ?? (cfg.type === 'bar' ? 'bar' : 'line');
  const hasBars = cfg.series.some((s) => kindOf(s) === 'bar') || cfg.type === 'bar';
  const inset = cfg.horizontal ? undefined : cfg.inset;
  const boundaryGap = hasBars || !!inset;
  const edgeLabels = cfg.compact && !boundaryGap ? { alignMinLabel: 'left' as const, alignMaxLabel: 'right' as const } : {};
  const categoryLabelTruncationOptions = cfg.compact && cfg.horizontal ? {} : { overflow: 'truncate' as const, width: MAX_CATEGORY_AXIS_LABEL_WIDTH_PIXELS };
  const categoryLabel = { ...axisLabel(theme), hideOverlap: true, ...categoryLabelTruncationOptions, ...edgeLabels };
  const categoryAxis = { type: 'category' as const, data: cfg.categories ?? [], inverse: !!cfg.horizontal, boundaryGap, axisLabel: categoryLabel, axisLine: axisLine(theme) };
  // Two value axes tick independently, so each draws its own grid: one set of
  // lines, and a shared tick count so the right-hand labels land on them.
  const valueAxis = (title: string | undefined, fmt: (v: number) => string, opposite = false, max?: number) => {
    const axisSeries = cfg.series.filter((s) => (s.axis === 'right') === opposite);
    const bytesPerValue = bytesPerFormatterInput.get(fmt);
    const byteTicks = bytesPerValue === undefined ? undefined : getByteAxisIntervalAndMax(axisSeries.flatMap(valuesOf), max, bytesPerValue);
    return {
      type: 'value' as const,
      name: (!cfg.compact && title) || undefined,
      min: (extent: { min: number }) => Math.min(0, extent.min),
      max: byteTicks?.max ?? max,
      ...(areAllSeriesValuesWholeNumbers(axisSeries) ? { minInterval: 1 } : {}),
      ...(byteTicks ? { interval: byteTicks.interval } : {}),
      nameTextStyle: { color: theme.text },
      axisLabel: { ...axisLabel(theme), hideOverlap: true, formatter: (v: number) => fmt(v) },
      axisLine: axisLine(theme),
      splitNumber: cfg.compact ? COMPACT_AXIS_TICKS : AXIS_TICKS,
      splitLine: opposite ? { show: false } : splitLine(theme),
      ...(opposite ? { position: 'right' as const } : {}),
    };
  };

  const secondary = cfg.horizontal ? undefined : cfg.secondary;
  const secondaryFormat = secondary?.valueFormatter ?? format;
  const primary = valueAxis(cfg.yAxisTitle, format, false, cfg.yAxisMax);
  const values = secondary ? [primary, valueAxis(secondary.title, secondaryFormat, true)] : primary;
  const axisOf = (s: MetricSeries) => (s.axis === 'right' ? 1 : 0);

  return {
    grid: gridOf(cfg, inset, !!secondary),
    xAxis: cfg.horizontal ? primary : categoryAxis,
    yAxis: cfg.horizontal ? categoryAxis : values,
    ...(secondary ? { tooltip: dualAxisTooltip(cfg, format, secondaryFormat, theme) } : {}),
    series: cfg.series.map((s) => {
      const data = valuesOf(s);
      const kind = kindOf(s);
      const showSymbol = (s.type === 'line' && data.includes(null)) || hasIsolatedNonNullValue(data);
      return {
        name: s.name,
        type: kind,
        data,
        ...(secondary ? { yAxisIndex: axisOf(s) } : {}),
        ...(kind === 'bar' ? { barMaxWidth: 28 } : { showSymbol, lineStyle: { width: 2, ...(s.isLineDashed ? { type: 'dashed' as const } : {}) } }),
        ...(cfg.type === 'area' && kind === 'line' ? { areaStyle: { opacity: 0.25 } } : {}),
      };
    }),
  };
}

function gridOf(cfg: MetricChartConfig, inset: MetricChartConfig['inset'], secondary: boolean) {
  if (cfg.compact) return { left: 4, right: 4, top: 8, bottom: cfg.series.length > 1 ? 44 : 4, containLabel: true };
  if (inset) return { left: inset.left, right: inset.right, top: 28, bottom: 24, containLabel: false };
  return { left: 8, right: secondary ? 8 : 12, top: 28, bottom: 4, containLabel: true };
}

function getByteAxisIntervalAndMax(values: (number | null)[], fixedMax: number | undefined, bytesPerValue: number) {
  const max = fixedMax ?? values.reduce<number>((m, v) => (v !== null && v > m ? v : m), 0);
  if (!(max > 0)) return undefined;
  const maxBytes = max * bytesPerValue;
  let unit = 1;
  while (maxBytes / unit >= 1024) unit *= 1024;
  const rawStep = maxBytes / unit / AXIS_TICKS;
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const step = [1, 2, 5, 10].map((f) => f * magnitude).find((s) => s >= rawStep) ?? 10 * magnitude;
  const interval = ((unit === 1 ? Math.max(1, step) : step) * unit) / bytesPerValue;
  return { interval, max: fixedMax ?? Math.ceil(max / interval) * interval };
}

function dualAxisTooltip(
  cfg: MetricChartConfig,
  left: (v: number) => string,
  right: (v: number) => string,
  theme: ChartTheme,
) {
  return {
    ...tooltipChrome(theme),
    trigger: 'axis' as const,
    // ECharts does not export a usable public type for the axis tooltip payload.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    formatter: (params: any) => {
      const rows = (Array.isArray(params) ? params : [params]).map((p) => {
        const fmt = cfg.series[p.seriesIndex]?.axis === 'right' ? right : left;
        return `${p.marker}${p.seriesName}: ${fmt(Number(p.value))}`;
      });
      return [(Array.isArray(params) ? params[0] : params)?.axisValueLabel, ...rows].filter(Boolean).join('<br/>');
    },
  };
}

function radarOption(cfg: MetricChartConfig, theme: ChartTheme): EChartsOption {
  const indicator = (cfg.categories ?? []).map((name) => ({ name }));
  const series = indicator.length ? cfg.series : [];
  return {
    radar: {
      indicator,
      axisName: { color: theme.text, fontSize: 12, fontFamily: theme.mono },
      splitLine: { lineStyle: { color: theme.grid } },
      splitArea: { show: false },
      axisLine: { lineStyle: { color: theme.grid } },
    },
    series: [
      {
        type: 'radar',
        data: series.map((s) => ({
          name: s.name,
          value: valuesOf(s) as number[],
          areaStyle: { opacity: 0.2 },
        })),
      },
    ],
  };
}

function heatmapOption(
  cfg: MetricChartConfig,
  format: (v: number) => string,
  theme: ChartTheme,
): EChartsOption {
  const first = cfg.series[0]?.data ?? [];
  const xs = isXY(first) ? first.map((p) => p.x) : (cfg.categories ?? []);
  const data: [number, number, number][] = [];
  cfg.series.forEach((s, y) => {
    if (!isXY(s.data)) return;
    s.data.forEach((p, x) => p.y > 0 && data.push([x, y, p.y]));
  });
  const max = data.reduce((m, [, , v]) => Math.max(m, v), 0);

  return {
    grid: { left: 8, right: 12, top: 28, bottom: 36, containLabel: true },
    xAxis: { type: 'category', data: xs, axisLabel: axisLabel(theme), axisLine: axisLine(theme) },
    yAxis: { type: 'category', data: cfg.series.map((s) => s.name), axisLabel: axisLabel(theme), axisLine: axisLine(theme) },
    visualMap: {
      min: 0,
      max,
      orient: 'horizontal',
      left: 'center',
      bottom: 0,
      itemWidth: 10,
      itemHeight: 160,
      text: [format(max), format(0)],
      textStyle: { color: theme.text },
      inRange: { color: [cfg.colors?.[0] ?? theme.palette[0]], colorAlpha: [0.25, 1] },
      formatter: (v) => format(Number(v)),
    },
    series: [{ type: 'heatmap', data, progressive: 0, itemStyle: { borderColor: theme.grid, borderWidth: 1 } }],
  };
}
