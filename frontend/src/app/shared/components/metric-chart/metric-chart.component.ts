/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, OnDestroy, afterNextRender, booleanAttribute, effect, input, numberAttribute, viewChild } from '@angular/core';
import * as echarts from 'echarts/core';
import { BarChart, HeatmapChart, LineChart, RadarChart } from 'echarts/charts';
import { GridComponent, LegendComponent, RadarComponent, TooltipComponent, VisualMapComponent } from 'echarts/components';
import { SVGRenderer } from 'echarts/renderers';
import { MetricChartConfig, MetricChartType, MetricSeries, buildMetricChartOption } from './metric-chart.options';

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
  selector: 'app-metric-chart',
  standalone: true,
  template: `
    <div class="metric-chart" [class.metric-chart--bare]="bare()">
      @if (title() && !bare()) {
        <h3>{{ title() }}</h3>
      }
      <div #host class="metric-chart__plot" [style.height.px]="height()"></div>
    </div>
  `,
  styles: [
    `
      .metric-chart {
        background: #21262d;
        border: 1px solid #2d333b;
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
        color: #fff;
        font-size: 0.9rem;
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
    return buildMetricChartOption({
      type: this.type(),
      series: this.series(),
      categories: this.categories(),
      colors: this.colors(),
      horizontal: this.horizontal(),
      yAxisTitle: this.yAxisTitle(),
      valueFormatter: this.valueFormatter(),
      secondary: this.secondary(),
    });
  }
}
