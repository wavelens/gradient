/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Subscription } from 'rxjs';
import { BoardService, MetricPoint } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-overview',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <div class="kpis">
      <div class="kpi"><span class="label">Connected workers</span><span class="value">{{ workers() }}</span></div>
      <div class="kpi"><span class="label">Jobs pending</span><span class="value">{{ pending() }}</span></div>
      <div class="kpi"><span class="label">Jobs active</span><span class="value">{{ active() }}</span></div>
      <div class="kpi"><span class="label">Dispatched (live)</span><span class="value">{{ dispatchedCount() }}</span></div>
    </div>

    <app-metric-chart
      title="Builds completed per hour (24h)"
      type="area"
      [series]="completedSeries()"
      [categories]="categories()"
      [colors]="['#28a745']"
    ></app-metric-chart>
  `,
  styles: [
    `
      .kpis {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
        gap: 1rem;
        margin-bottom: 1.5rem;
      }
      .kpi {
        background: #21262d;
        border: 1px solid #2d333b;
        border-radius: 8px;
        padding: 1rem;
        display: flex;
        flex-direction: column;
        gap: 0.25rem;
      }
      .kpi .label {
        color: #abb0b4;
        font-size: 0.8rem;
      }
      .kpi .value {
        color: #fff;
        font-size: 1.6rem;
        font-weight: 600;
      }
    `,
  ],
})
export class BoardOverviewComponent implements OnInit, OnDestroy {
  private board = inject(BoardService);
  private live = inject(BoardLiveService);
  private sub?: Subscription;

  workers = signal(0);
  pending = signal(0);
  active = signal(0);
  dispatchedCount = signal(0);
  completedSeries = signal<{ name: string; data: number[] }[]>([]);
  categories = signal<string[]>([]);

  ngOnInit(): void {
    this.board.getWorkers().subscribe((w) => this.workers.set(w.length));
    this.board.getDispatchedJobs().subscribe((r) => this.dispatchedCount.set(r.jobs.length + r.other_running));
    this.board.query('builds.completed', 'hour').subscribe((points) => this.applyCompleted(points));
    this.sub = this.live.connect().subscribe({
      next: (ev) => {
        if (ev.type === 'queue_depth') {
          this.workers.set(ev.workers ?? this.workers());
          this.pending.set(ev.pending ?? this.pending());
          this.active.set(ev.active ?? this.active());
        } else if (ev.type === 'job_dispatched') {
          this.dispatchedCount.update((n) => n + 1);
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
