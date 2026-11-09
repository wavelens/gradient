/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService } from '@core/services/board.service';
import { LoadStateComponent } from '@gradient/ui/ui';
import { MetricChartComponent } from '@shared/ui';
import { firstLoad } from '../first-load';
import { clockTime, formatDuration } from '@shared/text';
import { MetricPoint, DurationsHeatmap } from '@core/models';
import { metricPointsTo24HourChartSeries } from '../hourly-metric-points-to-24-hour-slots';

@Component({
  selector: 'app-board-durations',
  standalone: true,
  imports: [CommonModule, MetricChartComponent, LoadStateComponent],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading durations..." errorTitle="Failed to load durations" (retry)="retry()">
      <gr-metric-chart
        title="Build duration distribution (count by band × hour)"
        doc="ui/job-board/"
        type="heatmap"
        [height]="300"
        [series]="heatmapSeries()"
        [colors]="['--gr-chart-cyan']"
        [error]="heatmap() === null"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Build duration (hourly avg vs max)"
        doc="ui/job-board/"
        type="area"
        [series]="buildChart().series"
        [categories]="buildChart().categories"
        [colors]="['--gr-chart-cyan', '--gr-graph-danger']"
        [valueFormatter]="duration"
        [error]="build() === null"
      ></gr-metric-chart>
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './durations.component.scss',
})
export class BoardDurationsComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();

  protected build = signal<MetricPoint[] | null>(null);
  protected heatmap = signal<DurationsHeatmap | null>(null);

  readonly duration = formatDuration;

  heatmapSeries = computed(() => {
    const h = this.heatmap();
    if (!h) return [];
    const times = h.times.map((t) => clockTime(t));
    return h.bands.map((b) => ({
      name: b.band,
      data: b.counts.map((c, i) => ({ x: times[i] ?? '', y: c })),
    }));
  });

  buildChart = computed(() =>
    metricPointsTo24HourChartSeries([
      { name: 'avg', points: this.build() ?? [], pick: (p) => p.avg, missing: null },
      { name: 'max', points: this.build() ?? [], pick: (p) => p.max, missing: null },
    ]),
  );

  ngOnInit(): void {
    this.load();
  }

  retry(): void {
    this.first.restart();
    this.load();
  }

  private load(): void {
    this.board.query('builds.duration_ms', 'hour').pipe(this.first.track()).subscribe((p) => this.build.set(p));
    this.board.getDurationsHeatmap(24).pipe(this.first.track()).subscribe((h) => this.heatmap.set(h));
  }
}
