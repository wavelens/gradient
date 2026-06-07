/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Router, RouterModule } from '@angular/router';
import { Subscription } from 'rxjs';
import { BoardService, DispatchedJobSummary } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';

@Component({
  selector: 'app-board-live-jobs',
  standalone: true,
  imports: [CommonModule, RouterModule],
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
          <tr [class.live]="isLive(j)" (click)="inspect(j)">
            <td>{{ j.kind === 1 ? 'build' : 'eval' }}</td>
            <td class="mono">{{ j.worker_id }}</td>
            <td>{{ j.score | number: '1.1-1' }}</td>
            <td>{{ j.dispatched_at | date: 'HH:mm:ss' }}</td>
            <td>{{ isLive(j) ? '' : '›' }}</td>
          </tr>
        } @empty {
          <tr><td colspan="5" class="muted">No visible dispatched jobs.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      .banner { color: #abb0b4; margin-bottom: 1rem; }
      .muted { color: #818181; }
      table.jobs { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      tbody tr:not(.live) { cursor: pointer; }
      tbody tr:not(.live):hover { background: #2d333b; }
      tbody tr.live { opacity: 0.7; font-style: italic; }
      .mono { font-family: monospace; }
    `,
  ],
})
export class BoardLiveJobsComponent implements OnInit, OnDestroy {
  private board = inject(BoardService);
  private live = inject(BoardLiveService);
  private router = inject(Router);
  private sub?: Subscription;

  jobs = signal<DispatchedJobSummary[]>([]);
  otherRunning = signal(0);

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
                id: `live:${ev.evaluation_id}:${ev.worker_id}:${Date.now()}`,
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

  isLive(j: DispatchedJobSummary): boolean {
    return j.id.startsWith('live:');
  }

  inspect(j: DispatchedJobSummary): void {
    if (!this.isLive(j)) {
      this.router.navigate(['/board/jobs', j.id]);
    }
  }
}
