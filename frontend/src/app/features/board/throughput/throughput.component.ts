/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { forkJoin } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import { LoadStateComponent } from '@gradient/ui/ui';
import { MetricChartComponent } from '@shared/ui';
import { firstLoad } from '../first-load';
import { workerAxisLabel } from '../worker-label';
import { clockTime } from '@shared/text';
import { BoardWorker, MetricPoint } from '@core/models';

@Component({
  selector: 'app-board-throughput',
  standalone: true,
  imports: [CommonModule, MetricChartComponent, LoadStateComponent],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading throughput..." errorTitle="Failed to load throughput" (retry)="retry()">
      <gr-metric-chart
        title="Build pipeline (hourly)"
        doc="ui/job-board/"
        type="line"
        [series]="buildSeries() ?? []"
        [categories]="buildCategories()"
        [colors]="['--gr-chart-cyan', '--gr-graph-success', '--gr-graph-danger']"
        [error]="buildSeries() === null"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Evaluations (hourly)"
        doc="ui/job-board/"
        type="line"
        [series]="evalSeries() ?? []"
        [categories]="evalCategories()"
        [colors]="['--gr-graph-success', '--gr-graph-danger']"
        [error]="evalSeries() === null"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Active jobs per worker"
        doc="ui/job-board/"
        type="bar"
        [series]="workerSeries() ?? []"
        [categories]="workerCategories()"
        [colors]="['--gr-chart-orange']"
        [error]="workerSeries() === null"
      ></gr-metric-chart>
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './throughput.component.scss',
})
export class BoardThroughputComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();

  buildCategories = signal<string[]>([]);
  buildSeries = signal<{ name: string; data: number[] }[] | null>(null);
  evalCategories = signal<string[]>([]);
  evalSeries = signal<{ name: string; data: number[] }[] | null>(null);
  workerCategories = signal<string[]>([]);
  workerSeries = signal<{ name: string; data: number[] }[] | null>(null);

  ngOnInit(): void {
    this.load();
  }

  retry(): void {
    this.first.restart();
    this.load();
  }

  private load(): void {
    forkJoin({
      created: this.board.query('builds.created', 'hour'),
      completed: this.board.query('builds.completed', 'hour'),
      failed: this.board.query('builds.failed', 'hour'),
    })
      .pipe(this.first.track())
      .subscribe(({ created, completed, failed }) => {
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
    })
      .pipe(this.first.track())
      .subscribe(({ completed, failed }) => {
        const { categories, series } = align([
          ['completed', completed],
          ['failed', failed],
        ]);
        this.evalCategories.set(categories);
        this.evalSeries.set(series);
      });

    this.board.getWorkers().pipe(this.first.track()).subscribe((workers) => this.applyWorkers(workers));
  }

  private applyWorkers(workers: BoardWorker[]): void {
    const named = workers.filter((w) => w.id !== null);
    this.workerCategories.set(named.map((w) => workerAxisLabel(w.name, w.id)));
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
  return { categories: ordered.map((b) => clockTime(b)), series };
}
