/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EChartsOption } from 'echarts';

export type MetricChartType = 'area' | 'bar' | 'line' | 'radar' | 'heatmap';

export type MetricPointXY = { x: string; y: number };
export type MetricAxis = 'left' | 'right';
export type MetricSeries = { name: string; data: (number | null)[] | MetricPointXY[]; axis?: MetricAxis };

export interface MetricChartConfig {
  type: MetricChartType;
  series: MetricSeries[];
  categories?: string[];
  colors?: string[];
  horizontal?: boolean;
  yAxisTitle?: string;
  valueFormatter?: (value: number) => string;
  secondary?: { title?: string; valueFormatter?: (value: number) => string };
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

const axisLabel = (t: ChartTheme) => ({ color: t.text, fontSize: 12, fontFamily: t.mono });
const axisLine = (t: ChartTheme) => ({ lineStyle: { color: t.border } });
const AXIS_TICKS = 4;
const splitLine = (t: ChartTheme) => ({ lineStyle: { color: t.grid, type: 'dashed' as const } });

/// Builds the full ECharts option for `gr-metric-chart`. Kept pure and free of
/// Angular/DOM so every chart shape is unit-testable without a renderer.
export function buildMetricChartOption(cfg: MetricChartConfig, theme: ChartTheme): EChartsOption {
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
      top: 0,
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

function cartesianOption(cfg: MetricChartConfig, format: (v: number) => string, theme: ChartTheme): EChartsOption {
  const categoryAxis = { type: 'category' as const, data: cfg.categories ?? [], boundaryGap: cfg.type === 'bar', axisLabel: axisLabel(theme), axisLine: axisLine(theme) };
  // Two value axes tick independently, so each draws its own grid: one set of
  // lines, and a shared tick count so the right-hand labels land on them.
  const valueAxis = (title: string | undefined, fmt: (v: number) => string, opposite = false) => ({
    type: 'value' as const,
    name: title || undefined,
    nameTextStyle: { color: theme.text },
    axisLabel: { ...axisLabel(theme), formatter: (v: number) => fmt(v) },
    axisLine: axisLine(theme),
    splitNumber: AXIS_TICKS,
    splitLine: opposite ? { show: false } : splitLine(theme),
    ...(opposite ? { position: 'right' as const } : {}),
  });

  const secondary = cfg.horizontal ? undefined : cfg.secondary;
  const secondaryFormat = secondary?.valueFormatter ?? format;
  const primary = valueAxis(cfg.yAxisTitle, format);
  const values = secondary ? [primary, valueAxis(secondary.title, secondaryFormat, true)] : primary;
  const axisOf = (s: MetricSeries) => (s.axis === 'right' ? 1 : 0);

  return {
    grid: { left: 8, right: secondary ? 8 : 12, top: 28, bottom: 4, containLabel: true },
    xAxis: cfg.horizontal ? primary : categoryAxis,
    yAxis: cfg.horizontal ? categoryAxis : values,
    ...(secondary ? { tooltip: dualAxisTooltip(cfg, format, secondaryFormat, theme) } : {}),
    series: cfg.series.map((s) => ({
      name: s.name,
      type: cfg.type === 'bar' ? ('bar' as const) : ('line' as const),
      data: (isXY(s.data) ? s.data.map((p) => p.y) : s.data) as (number | null)[],
      ...(secondary ? { yAxisIndex: axisOf(s) } : {}),
      ...(cfg.type === 'bar'
        ? { barMaxWidth: 28 }
        : { smooth: true, showSymbol: false, lineStyle: { width: 2 } }),
      ...(cfg.type === 'area' ? { areaStyle: { opacity: 0.25 } } : {}),
    })),
  };
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
  return {
    radar: {
      indicator: (cfg.categories ?? []).map((name) => ({ name })),
      axisName: { color: theme.text, fontSize: 12, fontFamily: theme.mono },
      splitLine: { lineStyle: { color: theme.grid } },
      splitArea: { show: false },
      axisLine: { lineStyle: { color: theme.grid } },
    },
    series: [
      {
        type: 'radar',
        data: cfg.series.map((s) => ({
          name: s.name,
          value: (isXY(s.data) ? s.data.map((p) => p.y) : s.data) as number[],
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
    s.data.forEach((p, x) => data.push([x, y, p.y]));
  });

  return {
    grid: { left: 8, right: 12, top: 28, bottom: 44, containLabel: true },
    xAxis: { type: 'category', data: xs, axisLabel: axisLabel(theme), axisLine: axisLine(theme), splitArea: { show: true } },
    yAxis: { type: 'category', data: cfg.series.map((s) => s.name), axisLabel: axisLabel(theme), axisLine: axisLine(theme), splitArea: { show: true } },
    visualMap: {
      min: 0,
      max: data.reduce((m, [, , v]) => Math.max(m, v), 0),
      calculable: true,
      orient: 'horizontal',
      left: 'center',
      bottom: 0,
      textStyle: { color: theme.text },
      inRange: { color: [theme.surface, cfg.colors?.[0] ?? theme.palette[0]] },
      formatter: (v) => format(Number(v)),
    },
    series: [{ type: 'heatmap', data, progressive: 0, itemStyle: { borderColor: theme.grid, borderWidth: 1 } }],
  };
}
