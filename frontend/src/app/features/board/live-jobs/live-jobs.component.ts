/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Subscription } from 'rxjs';
import {
  BoardService,
  DispatchedJobSummary,
  DispatchedJobDetail,
} from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-live-jobs',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <div class="banner">
      Showing {{ jobs().length }} dispatched job(s) you can see.
      @if (otherRunning() > 0) {
        <span class="muted">+ {{ otherRunning() }} other running (hidden).</span>
      }
    </div>

    <table class="jobs">
      <thead>
        <tr><th>Kind</th><th>Worker</th><th>Score</th><th>Dispatched</th><th></th></tr>
      </thead>
      <tbody>
        @for (j of jobs(); track j.id) {
          <tr (click)="open(j)" [class.selected]="selected()?.id === j.id">
            <td>{{ j.kind === 1 ? 'build' : 'eval' }}</td>
            <td class="mono">{{ j.worker_id }}</td>
            <td>{{ j.score | number: '1.1-1' }}</td>
            <td>{{ j.dispatched_at | date: 'HH:mm:ss' }}</td>
            <td>›</td>
          </tr>
        } @empty {
          <tr><td colspan="5" class="muted">No visible dispatched jobs.</td></tr>
        }
      </tbody>
    </table>

    @if (selected(); as detail) {
      <div class="drawer">
        <h3>Scoring breakdown <button (click)="selected.set(null)">✕</button></h3>
        <div class="meta">
          <span>worker <b class="mono">{{ detail.worker_id }}</b></span>
          <span>total score <b>{{ detail.score | number: '1.2-2' }}</b></span>
          <span>eval <b class="mono">{{ detail.evaluation_id }}</b></span>
        </div>
        <app-metric-chart
          type="bar"
          [horizontal]="true"
          [height]="320"
          [series]="breakdownSeries()"
          [categories]="breakdownCategories()"
          [colors]="['#17a2b8']"
        ></app-metric-chart>
        <details>
          <summary>Job context</summary>
          <pre>{{ detail.job_context | json }}</pre>
        </details>
        <details>
          <summary>Worker context</summary>
          <pre>{{ detail.worker_context | json }}</pre>
        </details>
      </div>
    }
  `,
  styles: [
    `
      .banner { color: #abb0b4; margin-bottom: 1rem; }
      .muted { color: #818181; }
      table.jobs { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      tbody tr { cursor: pointer; }
      tbody tr:hover, tbody tr.selected { background: #2d333b; }
      .mono { font-family: monospace; }
      .drawer { margin-top: 1.5rem; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; }
      .drawer h3 { color: #fff; display: flex; justify-content: space-between; margin: 0 0 0.75rem; }
      .drawer h3 button { background: none; border: none; color: #abb0b4; cursor: pointer; font-size: 1rem; }
      .meta { display: flex; gap: 1.5rem; color: #abb0b4; margin-bottom: 1rem; font-size: 0.85rem; }
      .meta b { color: #fff; }
      details { margin-top: 0.75rem; color: #abb0b4; }
      pre { background: #0d1118; padding: 0.75rem; border-radius: 6px; overflow: auto; color: #abb0b4; font-size: 0.8rem; }
    `,
  ],
})
export class BoardLiveJobsComponent implements OnInit, OnDestroy {
  private board = inject(BoardService);
  private live = inject(BoardLiveService);
  private sub?: Subscription;

  jobs = signal<DispatchedJobSummary[]>([]);
  otherRunning = signal(0);
  selected = signal<DispatchedJobDetail | null>(null);

  breakdownSeries = computed(() => {
    const rules = this.selected()?.score_breakdown?.rules ?? {};
    return [{ name: 'contribution', data: Object.values(rules) }];
  });
  breakdownCategories = computed(() => Object.keys(this.selected()?.score_breakdown?.rules ?? {}));

  ngOnInit(): void {
    this.board.getDispatchedJobs().subscribe((r) => {
      this.jobs.set(r.jobs);
      this.otherRunning.set(r.other_running);
    });
    this.sub = this.live.connect().subscribe({
      next: (ev) => {
        if (ev.type === 'job_dispatched' && ev.organization) {
          this.jobs.update((list) =>
            [
              {
                id: `${ev.evaluation_id}:${ev.worker_id}:${Date.now()}`,
                kind: ev.kind ?? 0,
                organization: ev.organization!,
                worker_id: ev.worker_id ?? '',
                score: ev.score ?? 0,
                dispatched_at: new Date().toISOString(),
                build_id: ev.build_id ?? null,
                evaluation_id: ev.evaluation_id ?? '',
              },
              ...list,
            ].slice(0, 200)
          );
        }
      },
      error: () => {},
    });
  }

  ngOnDestroy(): void {
    this.sub?.unsubscribe();
  }

  open(j: DispatchedJobSummary): void {
    // Live-synthesized rows carry a composite id; only persisted rows have detail.
    if (j.id.includes(':')) {
      return;
    }
    this.board.getJob(j.id).subscribe((d) => this.selected.set(d));
  }
}
