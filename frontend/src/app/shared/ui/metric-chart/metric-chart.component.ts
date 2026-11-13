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
import { ChartTheme, MetricChartConfig, MetricChartType, MetricPointXY, MetricSeries, buildMetricChartOption } from './metric-chart.options';
import { ThemeService } from '@core/services/theme.service';
import { DocLink } from '@core/docs';
import { LabelHelpComponent } from '../label-help/label-help.component';
import { SkeletonComponent } from '@gradient/ui/ui';
import type { EChartsOption } from 'echarts';

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
      read('--gr-graph-success'),
      read('--gr-graph-warning'),
      read('--gr-chart-purple'),
      read('--gr-chart-cyan'),
      read('--gr-chart-pink'),
      read('--gr-chart-orange'),
      read('--gr-chart-teal'),
    ],
    reducedMotion: window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false,
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
            <h2>
              {{ title() }}
              @if (doc() || explanationText()) {
                <gr-label-help [doc]="doc()" [title]="'About ' + title()" [explanationText]="explanationText()" />
              }
            </h2>
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
        } @else if (error()) {
          <p class="metric-chart__placeholder metric-chart__empty metric-chart__error">Couldn't load</p>
        } @else if (empty()) {
          <p class="metric-chart__placeholder metric-chart__empty">No data</p>
        }
      </div>
    </div>
  `,
  host: { '[attr.title]': 'null' },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './metric-chart.component.scss',
})
export class MetricChartComponent implements OnDestroy {
  title = input('');
  explanationText = input<string>();
  doc = input<DocLink | undefined>(undefined);
  type = input<MetricChartType>('area');
  height = input(260, { transform: numberAttribute });
  horizontal = input(false, { transform: booleanAttribute });
  series = input<MetricSeries[]>([]);
  categories = input<string[]>([]);
  /// Callers pass color roles such as `--gr-chart-cyan`. The component can resolve each role with the theme, so series colors change with the theme.
  colors = input<string[]>([]);
  bare = input(false, { transform: booleanAttribute });
  loading = input(false, { transform: booleanAttribute });
  error = input(false, { transform: booleanAttribute });
  yAxisTitle = input('');
  yAxisMax = input<number | undefined>(undefined);
  valueFormatter = input<((value: number) => string) | undefined>(undefined);
  secondary = input<MetricChartConfig['secondary']>(undefined);
  inset = input<MetricChartConfig['inset']>(undefined);

  private host = viewChild.required<ElementRef<HTMLElement>>('host');
  private chart?: echarts.ECharts;
  private resize?: ResizeObserver;
  private optionWaitingForHostSize?: EChartsOption;
  private width = signal(Infinity);
  protected compact = computed(() => this.width() < COMPACT_BELOW_PX);
  protected empty = computed(() =>
    this.series().every((s) => (s.data as readonly (number | MetricPointXY | null)[]).every((point) => point === null)),
  );
  private theme = inject(ThemeService);

  constructor() {
    afterRenderEffect(() => {
      const option = this.option();
      if (this.chart) {
        this.chart.setOption(option, { replaceMerge: ['series', 'xAxis', 'yAxis', 'legend', 'radar'] });
        return;
      }
      this.optionWaitingForHostSize = option;
      this.observeHostSize();
    });
  }

  /// Hosts without size (a hidden tab, a panel during layout) trigger an ECharts warning and no drawing. The chart can start only at the first non-zero size of the host.
  private observeHostSize(): void {
    if (this.resize) return;
    const el = this.host().nativeElement;
    if (typeof ResizeObserver === 'undefined') {
      this.createChart(el);
      return;
    }
    this.resize = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      this.width.set(width);
      if (this.chart) this.chart.resize();
      else if (width > 0 && height > 0) this.createChart(el);
    });
    this.resize.observe(el);
  }

  private createChart(el: HTMLElement): void {
    this.chart = echarts.init(el, undefined, { renderer: 'svg' });
    if (this.optionWaitingForHostSize) this.chart.setOption(this.optionWaitingForHostSize, { notMerge: true });
    this.optionWaitingForHostSize = undefined;
  }

  ngOnDestroy(): void {
    this.resize?.disconnect();
    this.chart?.dispose();
  }

  private option() {
    this.theme.resolved();
    const style = getComputedStyle(document.documentElement);
    return buildMetricChartOption({
      type: this.type(),
      series: this.series(),
      categories: this.categories(),
      colors: this.colors().map((role) => style.getPropertyValue(role).trim()),
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
