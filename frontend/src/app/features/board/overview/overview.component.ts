/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService } from '@core/services/board.service';
import { CardGridComponent, LoadStateComponent, StatCardComponent } from '@gradient/ui/ui';
import { LiveStatusComponent, MetricChartComponent } from '@shared/ui';
import { firstLoad, requestFailedText } from '../first-load';
import { subscribeToBoardSocketWithReload } from '../board-socket';
import { MetricPoint } from '@core/models';
import { metricPointsTo24HourChartSeries } from '../hourly-metric-points-to-24-hour-slots';

const formatJobCountWithHiddenJobs = (shown: number, hidden: number): number | string =>
  hidden > 0 ? `${shown} (${hidden} on projects you can't view)` : shown;

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
        <gr-stat-card compact label="Connected workers" [value]="workers() ?? failed" />
        <gr-stat-card compact label="Pending jobs" [value]="pending() ?? failed" />
        <gr-stat-card compact label="Running jobs" [value]="active() ?? failed" />
      </gr-card-grid>

      <gr-metric-chart
        title="Builds completed per hour (24h)"
        doc="ui/job-board/"
        type="area"
        [series]="completedChart().series"
        [categories]="completedChart().categories"
        [colors]="['--gr-graph-success']"
        [error]="completed() === null"
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

  protected readonly failed = requestFailedText;

  workers = signal<number | null>(null);
  pending = signal<number | string | null>(null);
  active = signal<number | string | null>(null);
  completed = signal<MetricPoint[] | null>(null);
  completedChart = computed(() =>
    metricPointsTo24HourChartSeries([{ name: 'completed', points: this.completed() ?? [], pick: (p) => p.count, missing: 0 }]),
  );

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
      .subscribe((points) => this.completed.set(points));
  }

  private loadCounts(): void {
    this.board.getWorkers().pipe(this.first.track()).subscribe((w) => this.workers.set(w.length));
    this.board.getPendingJobs().pipe(this.first.track()).subscribe((r) => this.pending.set(formatJobCountWithHiddenJobs(r.jobs.length, r.other_pending)));
    this.board.getAssignedJobs().pipe(this.first.track()).subscribe((r) => this.active.set(formatJobCountWithHiddenJobs(r.jobs.length, r.other_running)));
  }
}
