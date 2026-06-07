/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Router, RouterModule } from '@angular/router';
import { Subscription } from 'rxjs';
import { BoardService, DispatchedJobSummary } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';

type KindFilter = 'all' | 'eval' | 'build';
type StatusFilter = 'all' | 'pending' | 'dispatched';

@Component({
  selector: 'app-board-live-jobs',
  standalone: true,
  imports: [CommonModule, FormsModule, RouterModule],
  template: `
    <div class="banner">
      Showing {{ filteredJobs().length }} of {{ jobs().length }} dispatched job(s) you can see.
      @if (otherRunning() > 0) {
        <span class="muted">+ {{ otherRunning() }} other running (hidden).</span>
      }
    </div>

    <div class="filters">
      <label>Type
        <select [ngModel]="kindFilter()" (ngModelChange)="kindFilter.set($event)">
          <option value="all">all</option>
          <option value="eval">eval</option>
          <option value="build">build</option>
        </select>
      </label>
      <label>Status
        <select [ngModel]="statusFilter()" (ngModelChange)="statusFilter.set($event)">
          <option value="all">all</option>
          <option value="pending">pending (live)</option>
          <option value="dispatched">dispatched</option>
        </select>
      </label>
      <label>Score min
        <input type="number" step="0.1" [ngModel]="scoreMin()" (ngModelChange)="scoreMin.set($event)" placeholder="—" />
      </label>
      <label>Score max
        <input type="number" step="0.1" [ngModel]="scoreMax()" (ngModelChange)="scoreMax.set($event)" placeholder="—" />
      </label>
    </div>

    <table class="jobs">
      <thead>
        <tr><th>Kind</th><th>Worker</th><th>Score</th><th>Dispatched</th><th></th></tr>
      </thead>
      <tbody>
        @for (j of filteredJobs(); track j.id) {
          <tr [class.live]="isLive(j)" [class.clickable]="canInspect(j)" (click)="inspect(j)">
            <td>{{ j.kind === 1 ? 'build' : 'eval' }}</td>
            <td class="mono">{{ j.worker_id }}</td>
            <td>{{ j.score | number: '1.1-1' }}</td>
            <td>{{ j.dispatched_at | date: 'HH:mm:ss' }}</td>
            <td>{{ canInspect(j) ? '›' : '' }}</td>
          </tr>
        } @empty {
          <tr><td colspan="5" class="muted">No matching dispatched jobs.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      .banner { color: #abb0b4; margin-bottom: 1rem; }
      .muted { color: #818181; }
      .filters { display: flex; gap: 1.5rem; margin-bottom: 1rem; flex-wrap: wrap; }
      .filters label { display: flex; flex-direction: column; gap: 0.25rem; color: #818181; font-size: 0.75rem; }
      .filters select, .filters input { background: #21262d; color: #fff; border: 1px solid #2d333b; border-radius: 4px; padding: 0.25rem 0.4rem; min-width: 7rem; }
      table.jobs { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      tbody tr.clickable { cursor: pointer; }
      tbody tr.clickable:hover { background: #2d333b; }
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

  kindFilter = signal<KindFilter>('all');
  statusFilter = signal<StatusFilter>('all');
  scoreMin = signal<number | null>(null);
  scoreMax = signal<number | null>(null);

  filteredJobs = computed(() => {
    const kind = this.kindFilter();
    const status = this.statusFilter();
    const min = this.scoreMin();
    const max = this.scoreMax();
    return this.jobs().filter((j) => {
      if (kind === 'eval' && j.kind !== 0) return false;
      if (kind === 'build' && j.kind !== 1) return false;
      if (status === 'pending' && !this.isLive(j)) return false;
      if (status === 'dispatched' && this.isLive(j)) return false;
      if (min !== null && j.score < min) return false;
      if (max !== null && j.score > max) return false;

      return true;
    });
  });

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

  canInspect(j: DispatchedJobSummary): boolean {
    if (!this.isLive(j)) return true;

    return !!(j.organization && (j.build_id || j.evaluation_id));
  }

  inspect(j: DispatchedJobSummary): void {
    if (!this.isLive(j)) {
      this.router.navigate(['/board/jobs', j.id]);
      return;
    }

    if (j.organization && j.build_id) {
      this.router.navigate(['/organization', j.organization, 'artefacts', j.build_id]);
    } else if (j.organization && j.evaluation_id) {
      this.router.navigate(['/organization', j.organization, 'log', j.evaluation_id]);
    }
  }
}
