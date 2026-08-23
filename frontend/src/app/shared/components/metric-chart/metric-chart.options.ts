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

export const CHART_THEME = {
  text: '#abb0b4',
  grid: '#2d333b',
  border: '#2d333b',
  surface: '#21262d',
} as const;

const DEFAULT_COLORS = ['#17a2b8', '#dc3545', '#28a745', '#fd7e14', '#6f42c1', '#e83e8c'];

const isXY = (d: MetricSeries['data']): d is MetricPointXY[] =>
  d.length > 0 && typeof d[0] === 'object' && d[0] !== null;

const axisLabel = { color: CHART_THEME.text, fontSize: 11 };
const axisLine = { lineStyle: { color: CHART_THEME.border } };
const splitLine = { lineStyle: { color: CHART_THEME.grid, type: 'dashed' as const } };

/// Builds the full ECharts option for `app-metric-chart`. Kept pure and free of
/// Angular/DOM so every chart shape is unit-testable without a renderer.
export function buildMetricChartOption(cfg: MetricChartConfig): EChartsOption {
  const format = cfg.valueFormatter ?? ((v: number) => String(v));
  const base: EChartsOption = {
    backgroundColor: 'transparent',
    color: cfg.colors?.length ? [...cfg.colors] : DEFAULT_COLORS,
    legend: {
      show: cfg.series.length > 1,
      textStyle: { color: CHART_THEME.text },
      top: 0,
    },
    tooltip: {
      trigger: cfg.type === 'heatmap' || cfg.type === 'radar' ? 'item' : 'axis',
      valueFormatter: (v) => format(Number(v)),
    },
  };

  if (cfg.type === 'radar') return { ...base, ...radarOption(cfg) };
  if (cfg.type === 'heatmap') return { ...base, ...heatmapOption(cfg, format) };
  return { ...base, ...cartesianOption(cfg, format) };
}

function cartesianOption(cfg: MetricChartConfig, format: (v: number) => string): EChartsOption {
  const categoryAxis = { type: 'category' as const, data: cfg.categories ?? [], boundaryGap: cfg.type === 'bar', axisLabel, axisLine };
  const valueAxis = (title: string | undefined, fmt: (v: number) => string, opposite = false) => ({
    type: 'value' as const,
    name: title || undefined,
    nameTextStyle: { color: CHART_THEME.text },
    axisLabel: { ...axisLabel, formatter: (v: number) => fmt(v) },
    axisLine,
    splitLine,
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
    ...(secondary ? { tooltip: dualAxisTooltip(cfg, format, secondaryFormat) } : {}),
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

function dualAxisTooltip(cfg: MetricChartConfig, left: (v: number) => string, right: (v: number) => string) {
  return {
    trigger: 'axis' as const,
    formatter: (params: any) => {
      const rows = (Array.isArray(params) ? params : [params]).map((p) => {
        const fmt = cfg.series[p.seriesIndex]?.axis === 'right' ? right : left;
        return `${p.marker}${p.seriesName}: ${fmt(Number(p.value))}`;
      });
      return [(Array.isArray(params) ? params[0] : params)?.axisValueLabel, ...rows].filter(Boolean).join('<br/>');
    },
  };
}

function radarOption(cfg: MetricChartConfig): EChartsOption {
  return {
    radar: {
      indicator: (cfg.categories ?? []).map((name) => ({ name })),
      axisName: { color: CHART_THEME.text, fontSize: 11 },
      splitLine: { lineStyle: { color: CHART_THEME.grid } },
      splitArea: { show: false },
      axisLine: { lineStyle: { color: CHART_THEME.grid } },
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

function heatmapOption(cfg: MetricChartConfig, format: (v: number) => string): EChartsOption {
  const first = cfg.series[0]?.data ?? [];
  const xs = isXY(first) ? first.map((p) => p.x) : (cfg.categories ?? []);
  const data: [number, number, number][] = [];
  cfg.series.forEach((s, y) => {
    if (!isXY(s.data)) return;
    s.data.forEach((p, x) => data.push([x, y, p.y]));
  });

  return {
    grid: { left: 8, right: 12, top: 28, bottom: 44, containLabel: true },
    xAxis: { type: 'category', data: xs, axisLabel, axisLine, splitArea: { show: true } },
    yAxis: { type: 'category', data: cfg.series.map((s) => s.name), axisLabel, axisLine, splitArea: { show: true } },
    visualMap: {
      min: 0,
      max: data.reduce((m, [, , v]) => Math.max(m, v), 0),
      calculable: true,
      orient: 'horizontal',
      left: 'center',
      bottom: 0,
      textStyle: { color: CHART_THEME.text },
      inRange: { color: [CHART_THEME.surface, cfg.colors?.[0] ?? DEFAULT_COLORS[0]] },
      formatter: (v) => format(Number(v)),
    },
    series: [{ type: 'heatmap', data, progressive: 0, itemStyle: { borderColor: CHART_THEME.grid, borderWidth: 1 } }],
  };
}
