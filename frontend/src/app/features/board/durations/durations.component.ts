/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, MetricPoint } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-durations',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <app-metric-chart
      title="Build duration (s, hourly avg vs max)"
      type="area"
      [series]="buildSeries()"
      [categories]="buildCategories()"
      [colors]="['#17a2b8', '#dc3545']"
    ></app-metric-chart>

    <app-metric-chart
      title="Dispatch wait (s, hourly avg)"
      type="area"
      [series]="waitSeries()"
      [categories]="waitCategories()"
      [colors]="['#6f42c1']"
    ></app-metric-chart>
  `,
  styles: [`app-metric-chart { display: block; margin-bottom: 1rem; }`],
})
export class BoardDurationsComponent implements OnInit {
  private board = inject(BoardService);

  private build = signal<MetricPoint[]>([]);
  private wait = signal<MetricPoint[]>([]);

  buildCategories = computed(() => this.build().map((p) => p.bucket_start.slice(11, 16)));
  buildSeries = computed(() => [
    { name: 'avg', data: this.build().map((p) => +(p.avg / 1000).toFixed(1)) },
    { name: 'max', data: this.build().map((p) => +(p.max / 1000).toFixed(1)) },
  ]);

  waitCategories = computed(() => this.wait().map((p) => p.bucket_start.slice(11, 16)));
  waitSeries = computed(() => [
    { name: 'avg', data: this.wait().map((p) => +(p.avg / 1000).toFixed(2)) },
  ]);

  ngOnInit(): void {
    this.board.query('builds.duration_ms', 'hour').subscribe((p) => this.build.set(p));
    this.board.query('dispatch.wait_ms', 'hour').subscribe((p) => this.wait.set(p));
  }
}
