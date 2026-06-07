/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, MetricPoint, DurationsHeatmap } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-durations',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <app-metric-chart
      title="Build duration distribution (count by band × hour)"
      type="heatmap"
      [height]="300"
      [series]="heatmapSeries()"
      [colors]="['#17a2b8']"
    ></app-metric-chart>

    <app-metric-chart
      title="Build duration (s, hourly avg vs max)"
      type="area"
      [series]="buildSeries()"
      [categories]="buildCategories()"
      [colors]="['#17a2b8', '#dc3545']"
    ></app-metric-chart>

    <app-metric-chart
      title="Wait (s, hourly avg): queue (excl. deps) vs dependency"
      type="area"
      [series]="waitSeries()"
      [categories]="waitCategories()"
      [colors]="['#6f42c1', '#fd7e14']"
    ></app-metric-chart>
  `,
  styles: [`app-metric-chart { display: block; margin-bottom: 1rem; }`],
})
export class BoardDurationsComponent implements OnInit {
  private board = inject(BoardService);

  private build = signal<MetricPoint[]>([]);
  private wait = signal<MetricPoint[]>([]);
  private deps = signal<MetricPoint[]>([]);
  private heatmap = signal<DurationsHeatmap | null>(null);

  heatmapSeries = computed(() => {
    const h = this.heatmap();
    if (!h) return [];
    const times = h.times.map((t) => t.slice(11, 16));
    return h.bands.map((b) => ({
      name: b.band,
      data: b.counts.map((c, i) => ({ x: times[i] ?? '', y: c })),
    }));
  });

  buildCategories = computed(() => this.build().map((p) => p.bucket_start.slice(11, 16)));
  buildSeries = computed(() => [
    { name: 'avg', data: this.build().map((p) => +(p.avg / 1000).toFixed(1)) },
    { name: 'max', data: this.build().map((p) => +(p.max / 1000).toFixed(1)) },
  ]);

  waitCategories = computed(() => this.wait().map((p) => p.bucket_start.slice(11, 16)));
  waitSeries = computed(() => {
    const depMap = new Map(this.deps().map((p) => [p.bucket_start, +(p.avg / 1000).toFixed(2)]));
    return [
      { name: 'queue wait (excl. deps)', data: this.wait().map((p) => +(p.avg / 1000).toFixed(2)) },
      { name: 'dependency wait', data: this.wait().map((p) => depMap.get(p.bucket_start) ?? 0) },
    ];
  });

  ngOnInit(): void {
    this.board.query('builds.duration_ms', 'hour').subscribe((p) => this.build.set(p));
    this.board.query('dispatch.wait_ms', 'hour').subscribe((p) => this.wait.set(p));
    this.board.query('deps.wait_ms', 'hour').subscribe((p) => this.deps.set(p));
    this.board.getDurationsHeatmap(24).subscribe((h) => this.heatmap.set(h));
  }
}
