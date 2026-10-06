/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Subscription } from 'rxjs';
import { BoardService, MetricPoint } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';
import { CardGridComponent, LoadingSpinnerComponent, StatCardComponent } from '@gradient/ui/ui';
import { MetricChartComponent } from '@shared/ui';
import { firstLoad } from '../first-load';

@Component({
  selector: 'app-board-overview',
  standalone: true,
  imports: [CommonModule, CardGridComponent, StatCardComponent, MetricChartComponent, LoadingSpinnerComponent],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading overview..." />
    } @else {
      <gr-card-grid class="kpis" min="180px">
        <gr-stat-card compact label="Connected workers" [value]="workers()" />
        <gr-stat-card compact label="Jobs pending" [value]="pending()" />
        <gr-stat-card compact label="Jobs active" [value]="active()" />
        <gr-stat-card compact label="Assigned (live)" [value]="assignedCount()" />
      </gr-card-grid>

      <gr-metric-chart
        title="Builds completed per hour (24h)"
        doc="ui/job-board/"
        type="area"
        [series]="completedSeries()"
        [categories]="categories()"
        [colors]="['#28a745']"
      ></gr-metric-chart>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './overview.component.scss',
})
export class BoardOverviewComponent implements OnInit, OnDestroy {
  private board = inject(BoardService);
  private live = inject(BoardLiveService);
  private sub?: Subscription;
  protected first = firstLoad();

  workers = signal(0);
  pending = signal(0);
  active = signal(0);
  assignedCount = signal(0);
  completedSeries = signal<{ name: string; data: number[] }[]>([]);
  categories = signal<string[]>([]);

  ngOnInit(): void {
    this.board.getWorkers().pipe(this.first.track()).subscribe((w) => this.workers.set(w.length));
    this.board
      .getAssignedJobs()
      .pipe(this.first.track())
      .subscribe((r) => this.assignedCount.set(r.jobs.length + r.other_running));
    this.board
      .query('builds.completed', 'hour')
      .pipe(this.first.track())
      .subscribe((points) => this.applyCompleted(points));
    this.sub = this.live.connect().subscribe({
      next: (ev) => {
        if (ev.event === 'worker.queue_depth') {
          this.workers.set(ev.content.workers ?? this.workers());
          this.pending.set(ev.content.pending ?? this.pending());
          this.active.set(ev.content.active ?? this.active());
        } else if (ev.event === 'worker.job_dispatched') {
          this.assignedCount.update((n) => n + 1);
        }
      },
      error: () => {},
    });
  }

  ngOnDestroy(): void {
    this.sub?.unsubscribe();
  }

  private applyCompleted(points: MetricPoint[]): void {
    this.categories.set(points.map((p) => p.bucket_start.slice(11, 16)));
    this.completedSeries.set([{ name: 'completed', data: points.map((p) => p.count) }]);
  }
}
