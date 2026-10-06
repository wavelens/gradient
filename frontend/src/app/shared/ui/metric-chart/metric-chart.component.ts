/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, OnDestroy, afterRenderEffect, booleanAttribute, computed, inject, input, numberAttribute, signal, viewChild, ChangeDetectionStrategy } from '@angular/core';
import * as echarts from 'echarts/core';
import { BarChart, HeatmapChart, LineChart, RadarChart } from 'echarts/charts';
import { GridComponent, LegendComponent, RadarComponent, TooltipComponent, VisualMapComponent } from 'echarts/components';
import { SVGRenderer } from 'echarts/renderers';
import { ChartTheme, MetricChartConfig, MetricChartType, MetricSeries, buildMetricChartOption } from './metric-chart.options';
import { ThemeService } from '@core/services/theme.service';
import { DocLink } from '@core/docs';
import { LabelHelpComponent } from '../label-help/label-help.component';
import { SkeletonComponent } from '@gradient/ui/ui';

/// Charts need concrete colours, so the semantic roles are read after each render: read during change
/// detection, the computed styles would settle on siblings whose bindings have not landed yet.
export function resolveChartTheme(): ChartTheme {
  const style = getComputedStyle(document.documentElement);
  const read = (name: string) => style.getPropertyValue(name).trim();
  return {
    text: read('--gr-text-secondary'),
    textStrong: read('--gr-text-primary'),
    muted: read('--gr-text-muted'),
    mono: getComputedStyle(document.body).getPropertyValue('--gr-font-mono') || "'Space Mono', monospace",
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

const COMPACT_BELOW_PX = 520;

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
  imports: [LabelHelpComponent, SkeletonComponent],
  template: `
    <div class="metric-chart" [class.metric-chart--bare]="bare()" [class.metric-chart--compact]="compact()" [attr.aria-busy]="loading() || null">
      @if (title() && !bare()) {
        <header class="metric-chart__header">
          <div>
            <h3>
              {{ title() }}
              @if (doc(); as link) {
                <gr-label-help [doc]="link" [title]="'About ' + title()" />
              }
            </h3>
            @if (subtitle()) {
              <p class="metric-chart__subtitle">{{ subtitle() }}</p>
            }
          </div>
          <div class="metric-chart__actions">
            <ng-content select="[slot=actions]"></ng-content>
          </div>
        </header>
      }
      <div class="metric-chart__body">
        <div #host class="metric-chart__plot" [class.metric-chart__plot--waiting]="loading()" [style.height.px]="height()"></div>
        @if (loading()) {
          <gr-skeleton class="metric-chart__placeholder" />
        }
      </div>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './metric-chart.component.scss',
})
export class MetricChartComponent implements OnDestroy {
  title = input('');
  subtitle = input('');
  doc = input<DocLink | undefined>(undefined);
  type = input<MetricChartType>('area');
  height = input(260, { transform: numberAttribute });
  horizontal = input(false, { transform: booleanAttribute });
  series = input<MetricSeries[]>([]);
  categories = input<string[]>([]);
  colors = input<string[]>([]);
  bare = input(false, { transform: booleanAttribute });
  loading = input(false, { transform: booleanAttribute });
  yAxisTitle = input('');
  yAxisMax = input<number | undefined>(undefined);
  valueFormatter = input<((value: number) => string) | undefined>(undefined);
  secondary = input<MetricChartConfig['secondary']>(undefined);
  inset = input<MetricChartConfig['inset']>(undefined);

  private host = viewChild.required<ElementRef<HTMLElement>>('host');
  private chart?: echarts.ECharts;
  private resize?: ResizeObserver;
  private width = signal(Infinity);
  protected compact = computed(() => this.width() < COMPACT_BELOW_PX);
  private theme = inject(ThemeService);

  constructor() {
    afterRenderEffect(() => {
      const option = this.option();
      this.chart ??= this.createChart();
      this.chart.setOption(option, { notMerge: true });
    });
  }

  private createChart(): echarts.ECharts {
    const el = this.host().nativeElement;
    const chart = echarts.init(el, undefined, { renderer: 'svg' });
    if (typeof ResizeObserver !== 'undefined') {
      this.resize = new ResizeObserver(([entry]) => {
        this.width.set(entry.contentRect.width);
        chart.resize();
      });
      this.resize.observe(el);
    }
    return chart;
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
      yAxisMax: this.yAxisMax(),
      valueFormatter: this.valueFormatter(),
      secondary: this.secondary(),
      inset: this.inset(),
      compact: this.compact(),
    }, resolveChartTheme());
  }
}
