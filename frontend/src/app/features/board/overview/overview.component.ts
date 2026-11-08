/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService } from '@core/services/board.service';
import { CardGridComponent, LoadStateComponent, StatCardComponent } from '@gradient/ui/ui';
import { LiveStatusComponent, MetricChartComponent } from '@shared/ui';
import { firstLoad } from '../first-load';
import { subscribeToBoardSocketWithReload } from '../board-socket';
import { clockTime } from '@shared/text';
import { MetricPoint } from '@core/models';

@Component({
  selector: 'app-board-overview',
  standalone: true,
  imports: [CommonModule, CardGridComponent, StatCardComponent, MetricChartComponent, LoadStateComponent, LiveStatusComponent],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading overview..." errorTitle="Failed to load overview" (retry)="retry()">
      @if (connected() !== null) {
        <gr-live-status [connected]="!!connected()" />
      }
      <gr-card-grid class="kpis" min="180px">
        <gr-stat-card compact label="Connected workers" [value]="workers()" />
        <gr-stat-card compact label="Jobs pending" [value]="pending()" />
        <gr-stat-card compact label="Jobs active" [value]="active()" />
      </gr-card-grid>

      <gr-metric-chart
        title="Builds completed per hour (24h)"
        doc="ui/job-board/"
        type="area"
        [series]="completedSeries()"
        [categories]="categories()"
        [colors]="['--gr-chart-green']"
      ></gr-metric-chart>
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './overview.component.scss',
})
export class BoardOverviewComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();
  protected connected = subscribeToBoardSocketWithReload(() => this.loadCounts());

  workers = signal(0);
  pending = signal(0);
  active = signal(0);
  completedSeries = signal<{ name: string; data: number[] }[]>([]);
  categories = signal<string[]>([]);

  ngOnInit(): void {
    this.load();
  }

  retry(): void {
    this.first.restart();
    this.load();
  }

  private load(): void {
    this.loadCounts();
    this.board
      .query('builds.completed', 'hour')
      .pipe(this.first.track())
      .subscribe((points) => this.applyCompleted(points));
  }

  private loadCounts(): void {
    this.board.getWorkers().pipe(this.first.track()).subscribe((w) => this.workers.set(w.length));
    this.board.getPendingJobs().pipe(this.first.track()).subscribe((r) => this.pending.set(r.jobs.length + r.other_pending));
    this.board.getAssignedJobs().pipe(this.first.track()).subscribe((r) => this.active.set(r.jobs.length + r.other_running));
  }

  private applyCompleted(points: MetricPoint[]): void {
    this.categories.set(points.map((p) => clockTime(p.bucket_start)));
    this.completedSeries.set([{ name: 'completed', data: points.map((p) => p.count) }]);
  }
}
