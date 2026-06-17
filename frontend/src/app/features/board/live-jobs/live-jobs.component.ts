/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal, computed, effect } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Router, RouterModule } from '@angular/router';
import { Subscription } from 'rxjs';
import { BoardService, DispatchedJobSummary, DispatchDecisionView, PendingJobSummary } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';

type KindFilter = 'all' | 'eval' | 'build';
type StatusFilter = 'all' | 'pending' | 'dispatched';
type ScoreScope = 'current' | 'all';

interface DecisionRow {
  at: string;
  worker_id: string;
  kind: number;
  pname: string | null;
  score: number;
  won: boolean;
}

@Component({
  selector: 'app-board-live-jobs',
  standalone: true,
  imports: [CommonModule, FormsModule, RouterModule],
  template: `
    <div class="view-toggle">
      <button [class.active]="view() === 'dispatched'" (click)="setView('dispatched')">Dispatched</button>
      <button [class.active]="view() === 'pending'" (click)="setView('pending')">Pending</button>
    </div>

    @if (view() === 'dispatched') {
      <div class="banner">
        Showing {{ filteredJobs().length }} of {{ jobs().length }} dispatched job(s) you can see.
        @if (otherRunning() > 0) {
          <span class="muted">+ {{ otherRunning() }} other running (hidden).</span>
        }
      </div>

      <div class="filters">
        <label>Scores
          <select [ngModel]="scoreScope()" (ngModelChange)="setScoreScope($event)">
            <option value="current">dispatched</option>
            <option value="all">incl. rejected</option>
          </select>
        </label>
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
          <input type="number" step="0.1" [ngModel]="scoreMin()" (ngModelChange)="scoreMin.set($event)" placeholder="-" />
        </label>
        <label>Score max
          <input type="number" step="0.1" [ngModel]="scoreMax()" (ngModelChange)="scoreMax.set($event)" placeholder="-" />
        </label>
      </div>

      @if (scoreScope() === 'current') {
        <table class="jobs">
          <thead>
            <tr><th>Kind</th><th>Worker</th><th>Derivation</th><th>Score</th><th>Dispatched</th><th></th></tr>
          </thead>
          <tbody>
            @for (j of filteredJobs(); track j.id) {
              <tr [class.live]="isLive(j)" [class.clickable]="canInspect(j)" (click)="inspect(j)">
                <td>{{ j.kind === 1 ? 'build' : 'eval' }}</td>
                <td class="mono">{{ j.worker_id }}</td>
                <td class="mono">{{ j.pname ?? '-' }}</td>
                <td>{{ j.score | number: '1.1-1' }}</td>
                <td>{{ j.dispatched_at | date: 'HH:mm:ss' }}</td>
                <td>{{ canInspect(j) ? '›' : '' }}</td>
              </tr>
            } @empty {
              <tr><td colspan="6" class="muted">No matching dispatched jobs.</td></tr>
            }
          </tbody>
        </table>
      } @else {
        <table class="jobs">
          <thead>
            <tr><th>Outcome</th><th>Kind</th><th>Worker</th><th>Derivation</th><th>Score</th><th>When</th></tr>
          </thead>
          <tbody>
            @for (r of decisionRows(); track $index) {
              <tr [class.negative]="r.score < 0">
                <td>{{ r.won ? 'dispatched' : 'passed over' }}</td>
                <td>{{ r.kind === 1 ? 'build' : 'eval' }}</td>
                <td class="mono">{{ r.worker_id }}</td>
                <td class="mono">{{ r.pname ?? '-' }}</td>
                <td>{{ r.score | number: '1.1-1' }}</td>
                <td>{{ r.at | date: 'HH:mm:ss' }}</td>
              </tr>
            } @empty {
              <tr><td colspan="6" class="muted">No recent decisions (superuser-only).</td></tr>
            }
          </tbody>
        </table>
      }
    }

    @if (view() === 'pending') {
      <div class="banner">
        {{ pendingJobs().length }} pending job(s) you can see.
        @if (otherPending() > 0) { <span class="muted">+ {{ otherPending() }} hidden.</span> }
      </div>
      <table class="jobs">
        <thead><tr><th>Kind</th><th>Evaluation</th><th>Derivation</th><th>Deps</th><th>Queued</th></tr></thead>
        <tbody>
          @for (p of pendingJobs(); track p.evaluation_id + (p.build_id ?? '')) {
            <tr class="clickable" [routerLink]="['/board/jobs', p.evaluation_id]">
              <td>{{ p.kind === 1 ? 'build' : 'eval' }}</td>
              <td class="mono">{{ p.evaluation_id.slice(0, 8) }}</td>
              <td class="mono">{{ p.pname ?? '-' }}</td>
              <td>{{ p.dependency_count }}</td>
              <td>{{ p.queued_at | date: 'HH:mm:ss' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="5" class="muted">No pending jobs.</td></tr>
          }
        </tbody>
      </table>
    }
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
      tbody tr.negative td { color: #f0883e; }
      .mono { font-family: monospace; }
      .view-toggle { display: flex; gap: 0.5rem; margin-bottom: 1rem; }
      .view-toggle button { background: #21262d; color: #abb0b4; border: 1px solid #2d333b; border-radius: 6px; padding: 0.3rem 0.8rem; cursor: pointer; }
      .view-toggle button.active { background: #2d333b; color: #fff; }
    `,
  ],
})
export class BoardLiveJobsComponent implements OnInit, OnDestroy {
  private static readonly STATE_KEY = 'board.live-jobs.filters';
  // Mirror the server's `board/jobs/dispatched` cap so optimistic live rows and
  // the reconciled API list converge instead of flipping the count.
  private static readonly MAX_DISPATCHED = 500;

  private board = inject(BoardService);
  private live = inject(BoardLiveService);
  private router = inject(Router);
  private sub?: Subscription;
  private refreshTimer?: ReturnType<typeof setTimeout>;

  jobs = signal<DispatchedJobSummary[]>([]);
  otherRunning = signal(0);

  view = signal<'dispatched' | 'pending'>('dispatched');
  pendingJobs = signal<PendingJobSummary[]>([]);
  otherPending = signal(0);

  kindFilter = signal<KindFilter>('all');
  statusFilter = signal<StatusFilter>('all');
  scoreMin = signal<number | null>(null);
  scoreMax = signal<number | null>(null);

  scoreScope = signal<ScoreScope>('current');
  decisions = signal<DispatchDecisionView[]>([]);

  constructor() {
    this.restoreState();
    effect(() => {
      const state = {
        view: this.view(),
        kindFilter: this.kindFilter(),
        statusFilter: this.statusFilter(),
        scoreMin: this.scoreMin(),
        scoreMax: this.scoreMax(),
        scoreScope: this.scoreScope(),
      };
      try {
        sessionStorage.setItem(BoardLiveJobsComponent.STATE_KEY, JSON.stringify(state));
      } catch {
        /* storage unavailable */
      }
    });
  }

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

  /// Recent decisions flattened to one row per candidate (incl. rejected and
  /// negative scores), honoring the type/score filters above.
  decisionRows = computed<DecisionRow[]>(() => {
    const kind = this.kindFilter();
    const min = this.scoreMin();
    const max = this.scoreMax();
    const rows: DecisionRow[] = [];
    for (const d of this.decisions()) {
      for (const c of d.candidates) {
        if (kind === 'eval' && c.kind !== 0) continue;
        if (kind === 'build' && c.kind !== 1) continue;
        if (min !== null && c.score < min) continue;
        if (max !== null && c.score > max) continue;
        rows.push({
          at: d.at,
          worker_id: d.worker_id,
          kind: c.kind,
          pname: c.pname,
          score: c.score,
          won: d.winner === c.job_id,
        });
      }
    }

    return rows;
  });

  ngOnInit(): void {
    this.loadDispatched();
    if (this.view() === 'pending') this.loadPending();
    if (this.scoreScope() === 'all') this.loadDecisions();
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
                pname: null,
              },
              ...list,
            ].slice(0, BoardLiveJobsComponent.MAX_DISPATCHED)
          );
          this.scheduleRefresh();
        }
      },
      error: () => {},
    });
  }

  ngOnDestroy(): void {
    this.sub?.unsubscribe();
    clearTimeout(this.refreshTimer);
  }

  setView(v: 'dispatched' | 'pending'): void {
    this.view.set(v);
    if (v === 'pending') this.loadPending();
  }

  setScoreScope(scope: ScoreScope): void {
    this.scoreScope.set(scope);
    if (scope === 'all') this.loadDecisions();
  }

  private loadDecisions(): void {
    this.board.getDispatchDecisions().subscribe({
      next: (d) => this.decisions.set(d),
      error: () => this.decisions.set([]),
    });
  }

  private loadDispatched(): void {
    this.board.getDispatchedJobs().subscribe((r) => {
      this.jobs.set(r.jobs);
      this.otherRunning.set(r.other_running);
    });
  }

  /// Reconcile the optimistic live rows with the persisted, selectable rows.
  /// Throttled so a busy board refreshes at most once per window instead of
  /// deferring forever under a steady event stream.
  private scheduleRefresh(): void {
    if (this.refreshTimer) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined;
      this.loadDispatched();
    }, 1500);
  }

  private restoreState(): void {
    try {
      const raw = sessionStorage.getItem(BoardLiveJobsComponent.STATE_KEY);
      if (!raw) return;
      const s = JSON.parse(raw);
      if (s.view) this.view.set(s.view);
      if (s.kindFilter) this.kindFilter.set(s.kindFilter);
      if (s.statusFilter) this.statusFilter.set(s.statusFilter);
      this.scoreMin.set(s.scoreMin ?? null);
      this.scoreMax.set(s.scoreMax ?? null);
      if (s.scoreScope) this.scoreScope.set(s.scoreScope);
    } catch {
      /* ignore malformed state */
    }
  }

  private loadPending(): void {
    this.board.getPendingJobs().subscribe((r) => {
      this.pendingJobs.set(r.jobs);
      this.otherPending.set(r.other_pending);
    });
  }

  isLive(j: DispatchedJobSummary): boolean {
    return j.id.startsWith('live:');
  }

  canInspect(j: DispatchedJobSummary): boolean {
    return !this.isLive(j);
  }

  inspect(j: DispatchedJobSummary): void {
    if (this.isLive(j)) return;
    this.router.navigate(['/board/jobs', j.id]);
  }
}
