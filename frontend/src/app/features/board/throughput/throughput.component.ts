/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { forkJoin } from 'rxjs';
import { BoardService, MetricPoint, BoardWorker } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-throughput',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <app-metric-chart
      title="Build pipeline (hourly)"
      type="line"
      [series]="buildSeries()"
      [categories]="buildCategories()"
      [colors]="['#17a2b8', '#28a745', '#dc3545']"
    ></app-metric-chart>

    <app-metric-chart
      title="Evaluations (hourly)"
      type="line"
      [series]="evalSeries()"
      [categories]="evalCategories()"
      [colors]="['#28a745', '#dc3545']"
    ></app-metric-chart>

    <app-metric-chart
      title="Active jobs per worker"
      type="bar"
      [series]="workerSeries()"
      [categories]="workerCategories()"
      [colors]="['#fd7e14']"
    ></app-metric-chart>
  `,
  styles: [`app-metric-chart { display: block; margin-bottom: 1rem; }`],
})
export class BoardThroughputComponent implements OnInit {
  private board = inject(BoardService);

  buildCategories = signal<string[]>([]);
  buildSeries = signal<{ name: string; data: number[] }[]>([]);
  evalCategories = signal<string[]>([]);
  evalSeries = signal<{ name: string; data: number[] }[]>([]);
  workerCategories = signal<string[]>([]);
  workerSeries = signal<{ name: string; data: number[] }[]>([]);

  ngOnInit(): void {
    forkJoin({
      created: this.board.query('builds.created', 'hour'),
      completed: this.board.query('builds.completed', 'hour'),
      failed: this.board.query('builds.failed', 'hour'),
    }).subscribe(({ created, completed, failed }) => {
      const { categories, series } = align([
        ['created', created],
        ['completed', completed],
        ['failed', failed],
      ]);
      this.buildCategories.set(categories);
      this.buildSeries.set(series);
    });

    forkJoin({
      completed: this.board.query('evals.completed', 'hour'),
      failed: this.board.query('evals.failed', 'hour'),
    }).subscribe(({ completed, failed }) => {
      const { categories, series } = align([
        ['completed', completed],
        ['failed', failed],
      ]);
      this.evalCategories.set(categories);
      this.evalSeries.set(series);
    });

    this.board.getWorkers().subscribe((workers) => this.applyWorkers(workers));
  }

  private applyWorkers(workers: BoardWorker[]): void {
    const named = workers.filter((w) => w.id !== null);
    this.workerCategories.set(named.map((w) => (w.id ?? '').slice(0, 12)));
    this.workerSeries.set([{ name: 'assigned', data: named.map((w) => w.assigned_jobs) }]);
  }
}

/// Merge several metric series onto a shared, sorted bucket axis, zero-filling
/// gaps so multi-line charts stay aligned even when sources have ragged buckets.
function align(
  named: [string, MetricPoint[]][]
): { categories: string[]; series: { name: string; data: number[] }[] } {
  const buckets = new Set<string>();
  for (const [, points] of named) {
    for (const p of points) buckets.add(p.bucket_start);
  }
  const ordered = [...buckets].sort();
  const series = named.map(([name, points]) => {
    const byBucket = new Map(points.map((p) => [p.bucket_start, p.count]));
    return { name, data: ordered.map((b) => byBucket.get(b) ?? 0) };
  });
  return { categories: ordered.map((b) => b.slice(11, 16)), series };
}
