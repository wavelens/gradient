/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, OnDestroy, afterNextRender, booleanAttribute, effect, inject, input, numberAttribute, viewChild, ChangeDetectionStrategy } from '@angular/core';
import * as echarts from 'echarts/core';
import { BarChart, HeatmapChart, LineChart, RadarChart } from 'echarts/charts';
import { GridComponent, LegendComponent, RadarComponent, TooltipComponent, VisualMapComponent } from 'echarts/components';
import { SVGRenderer } from 'echarts/renderers';
import { ChartTheme, MetricChartConfig, MetricChartType, MetricSeries, buildMetricChartOption } from './metric-chart.options';
import { ThemeService } from '@core/services/theme.service';

/// Charts need concrete colours, so the semantic roles are read once per render.
export function resolveChartTheme(): ChartTheme {
  const style = getComputedStyle(document.documentElement);
  const read = (name: string) => style.getPropertyValue(name).trim();
  return {
    text: read('--gr-text-secondary'),
    textStrong: read('--gr-text-primary'),
    muted: read('--gr-text-muted'),
    grid: read('--gr-border'),
    border: read('--gr-border'),
    surface: read('--gr-surface-raised'),
    palette: [
      read('--gr-graph-running'),
      read('--gr-graph-danger'),
      read('--gr-graph-success'),
      read('--gr-graph-warning'),
    ],
  };
}

echarts.use([
  BarChart,
  HeatmapChart,
  LineChart,
  RadarChart,
  GridComponent,
  LegendComponent,
  RadarComponent,
  TooltipComponent,
  VisualMapComponent,
  SVGRenderer,
]);

/// Dark-themed ECharts wrapper. `bare` drops the card chrome so callers that
/// already supply their own header and panel can reuse the same renderer.
@Component({
  selector: 'gr-metric-chart',
  standalone: true,
  template: `
    <div class="metric-chart" [class.metric-chart--bare]="bare()">
      @if (title() && !bare()) {
        <h3>{{ title() }}</h3>
      }
      <div #host class="metric-chart__plot" [style.height.px]="height()"></div>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: [
    `
      .metric-chart {
        background: var(--gr-surface-raised);
        border: 1px solid var(--gr-border);
        border-radius: 8px;
        padding: 1rem;
      }
      .metric-chart--bare {
        background: none;
        border: 0;
        border-radius: 0;
        padding: 0;
      }
      .metric-chart__plot {
        width: 100%;
      }
      h3 {
        color: var(--gr-text-primary);
        font-size: 0.875rem;
        margin: 0 0 0.5rem;
      }
    `,
  ],
})
export class MetricChartComponent implements OnDestroy {
  title = input('');
  type = input<MetricChartType>('area');
  height = input(260, { transform: numberAttribute });
  horizontal = input(false, { transform: booleanAttribute });
  series = input<MetricSeries[]>([]);
  categories = input<string[]>([]);
  colors = input<string[]>([]);
  bare = input(false, { transform: booleanAttribute });
  yAxisTitle = input('');
  valueFormatter = input<((value: number) => string) | undefined>(undefined);
  secondary = input<MetricChartConfig['secondary']>(undefined);

  private host = viewChild.required<ElementRef<HTMLElement>>('host');
  private chart?: echarts.ECharts;
  private resize?: ResizeObserver;
  private theme = inject(ThemeService);

  constructor() {
    afterNextRender(() => {
      const el = this.host().nativeElement;
      this.chart = echarts.init(el, undefined, { renderer: 'svg' });
      this.chart.setOption(this.option(), { notMerge: true });
      if (typeof ResizeObserver !== 'undefined') {
        this.resize = new ResizeObserver(() => this.chart?.resize());
        this.resize.observe(el);
      }
    });

    effect(() => {
      const option = this.option();
      this.chart?.setOption(option, { notMerge: true });
    });
  }

  ngOnDestroy(): void {
    this.resize?.disconnect();
    this.chart?.dispose();
  }

  private option() {
    this.theme.resolved();
    return buildMetricChartOption({
      type: this.type(),
      series: this.series(),
      categories: this.categories(),
      colors: this.colors(),
      horizontal: this.horizontal(),
      yAxisTitle: this.yAxisTitle(),
      valueFormatter: this.valueFormatter(),
      secondary: this.secondary(),
    }, resolveChartTheme());
  }
}
