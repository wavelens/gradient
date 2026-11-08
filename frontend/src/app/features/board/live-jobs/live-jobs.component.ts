/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, effect, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Router, RouterModule } from '@angular/router';
import { BoardService } from '@core/services/board.service';
import { LiveStatusComponent } from '@shared/ui';
import { AuthService } from '@core/services/auth.service';
import { ButtonComponent, LoadStateComponent, TableComponent } from '@gradient/ui/ui';
import { firstLoad } from '../first-load';
import { subscribeToBoardSocketWithReload } from '../board-socket';
import { BoardRow, JobKindName, collapseClusters, jobKindName, jobSubjectLabel } from '../job-kind';
import { DispatchedJobSummary, PendingJobSummary, AssignDecisionView } from '@core/models';
import { links } from '@shared/routing';
import { type WaitingExplanation, explainWaiting, waitingAccessFrom } from '@shared/evaluation';
import { relativeTime, ServerTimestampToLocalDateTimePipe } from '@shared/text';

type KindFilter = 'all' | JobKindName;
type ScoreScope = 'current' | 'all';

interface DecisionRow {
  id: string;
  at: string;
  worker: string;
  kind: number;
  subject: string;
  score: number;
  won: boolean;
}

@Component({
  selector: 'app-board-live-jobs',
  standalone: true,
  imports: [CommonModule, ServerTimestampToLocalDateTimePipe, FormsModule, RouterModule, ButtonComponent, TableComponent, LoadStateComponent, LiveStatusComponent],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading jobs..." errorTitle="Failed to load jobs" (retry)="retry()">
      <div class="view-toggle">
        <button [class.active]="view() === 'assigned'" (click)="setView('assigned')">Assigned</button>
        <button [class.active]="view() === 'pending'" (click)="setView('pending')">Pending</button>
        @if (connected() !== null) { <gr-live-status [connected]="!!connected()" /> }
      </div>

      @if (view() === 'assigned') {
        <div class="banner">
          Showing {{ assignedRows().length }} of {{ collapseClusters(jobs()).length }} assigned job(s) visible to this account.
          @if (otherRunning() > 0) {
            <span class="muted">+ {{ otherRunning() }} other running (hidden).</span>
          }
        </div>

        <div class="filters">
          @if (superuser()) {
            <label>Scores
              <select [ngModel]="scoreScope()" (ngModelChange)="setScoreScope($event)">
                <option value="current">assigned</option>
                <option value="all">incl. rejected</option>
              </select>
            </label>
          }
          <label>Type
            <select [ngModel]="kindFilter()" (ngModelChange)="kindFilter.set($event)">
              <option value="all">all</option>
              <option value="eval">eval</option>
              <option value="build">build</option>
              <option value="exec">exec</option>
            </select>
          </label>
          <label>Score min
            <input type="number" step="0.1" [ngModel]="scoreMin()" (ngModelChange)="scoreMin.set($event)" placeholder="-" />
          </label>
          <label>Score max
            <input type="number" step="0.1" [ngModel]="scoreMax()" (ngModelChange)="scoreMax.set($event)" placeholder="-" />
          </label>
        </div>

        @if (!showDecisions()) {
          <gr-table class="jobs">
            <thead>
              <tr><th>Kind</th><th>Worker</th><th>Derivation / Evaluation / Exec</th><th>Score</th><th>Assigned</th><th><span class="sr-only">Details</span></th></tr>
            </thead>
            <tbody>
              @for (row of assignedRows(); track row.job.id) {
                @let j = row.job;
                <tr class="clickable" (click)="inspect(j)">
                  @let subject = rowSubject(row);
                  <td>{{ jobKindName(j.kind) }}</td>
                  <td class="mono">{{ j.cluster_job ? subJobLabel(row.subJobs) : (j.worker_name ?? j.worker_id) }}</td>
                  <td class="mono subject" [title]="subject">{{ subject }}</td>
                  <td>{{ j.score | number: '1.1-1' }}</td>
                  <td>{{ j.dispatched_at | serverDate: 'HH:mm:ss' }}</td>
                  <td>›</td>
                </tr>
              } @empty {
                <tr><td colspan="6" class="muted">No matching assigned jobs.</td></tr>
              }
            </tbody>
          </gr-table>
        } @else {
          <gr-table class="jobs">
            <thead>
              <tr><th>Outcome</th><th>Kind</th><th>Worker</th><th>Derivation / Evaluation / Exec</th><th>Score</th><th>When</th><th><span class="sr-only">Details</span></th></tr>
            </thead>
            <tbody>
              @for (r of decisionRows(); track r.id) {
                <tr [class.negative]="r.score < 0" class="clickable" (click)="inspectDecision(r)">
                  <td>{{ r.won ? 'assigned' : 'passed over' }}</td>
                  <td>{{ jobKindName(r.kind) }}</td>
                  <td class="mono">{{ r.worker }}</td>
                  <td class="mono subject" [title]="r.subject">{{ r.subject }}</td>
                  <td>{{ r.score | number: '1.1-1' }}</td>
                  <td>{{ r.at | serverDate: 'HH:mm:ss' }}</td>
                  <td>›</td>
                </tr>
              } @empty {
                <tr><td colspan="7" class="muted">No recent decisions.</td></tr>
              }
            </tbody>
          </gr-table>
        }
      }

      @if (view() === 'pending') {
        <div class="banner">
          {{ pendingRows().length }} pending job(s) visible to this account.
          @if (otherPending() > 0) { <span class="muted">+ {{ otherPending() }} hidden.</span> }
        </div>
        <gr-table class="jobs">
          <thead><tr><th>Kind</th><th>Project / task</th><th>Derivation / Evaluation / Exec</th><th>Waiting for</th><th>Age</th></tr></thead>
          <tbody>
            @for (row of pendingRows(); track pendingKey(row.job)) {
              @let p = row.job;
              @let subject = rowSubject(row);
              <tr>
                <td>{{ jobKindName(p.kind) }}</td>
                <td class="origin">
                  @if (p.project_name; as project) {
                    <a [routerLink]="links.project(project)">{{ project }}</a>
                    @if (p.task; as task) {
                      / <a [routerLink]="links.task(project, task, p.evaluation_id)">{{ task }}</a>
                    }
                  } @else {
                    <span class="muted">-</span>
                  }
                </td>
                <td class="mono subject"><a [routerLink]="links.boardPendingJob(p)" [title]="subject">{{ subject }}</a></td>
                <td class="waiting">
                  @if (waitingFor(p); as wait) {
                    <div class="wait">
                      <span [title]="wait.detail">{{ wait.title }}</span>
                      @if (wait.action; as action) {
                        @if (action.kind === 'link') {
                          <a grButton size="small" severity="secondary" [label]="action.label" [icon]="action.icon" [routerLink]="action.link"></a>
                        }
                      } @else if (wait.hint) {
                        <span class="muted hint">{{ wait.hint }}</span>
                      }
                    </div>
                  } @else {
                    <span class="muted">Waiting</span>
                  }
                </td>
                <td class="age" [title]="p.queued_at | serverDate: 'medium'">{{ relativeTime(p.queued_at) }}</td>
              </tr>
            } @empty {
              <tr><td colspan="5" class="muted">No pending jobs.</td></tr>
            }
          </tbody>
        </gr-table>
      }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './live-jobs.component.scss',
})
export class BoardLiveJobsComponent implements OnInit {
  protected readonly links = links;
  private static readonly STATE_KEY = 'board.live-jobs.filters';

  private board = inject(BoardService);
  private router = inject(Router);
  private auth = inject(AuthService);
  protected first = firstLoad();
  protected connected = subscribeToBoardSocketWithReload(() => this.load());
  protected readonly jobKindName = jobKindName;
  protected readonly jobSubjectLabel = jobSubjectLabel;
  protected readonly collapseClusters = collapseClusters;
  protected readonly relativeTime = relativeTime;

  jobs = signal<DispatchedJobSummary[]>([]);
  otherRunning = signal(0);

  view = signal<'assigned' | 'pending'>('assigned');
  pendingJobs = signal<PendingJobSummary[]>([]);
  otherPending = signal(0);

  kindFilter = signal<KindFilter>('all');
  scoreMin = signal<number | null>(null);
  scoreMax = signal<number | null>(null);

  scoreScope = signal<ScoreScope>('current');
  decisions = signal<AssignDecisionView[]>([]);
  protected superuser = computed(() => this.auth.user()?.superuser === true);
  showDecisions = computed(() => this.superuser() && this.scoreScope() === 'all');

  constructor() {
    this.restoreState();
    effect(() => {
      const state = {
        view: this.view(),
        kindFilter: this.kindFilter(),
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
    const min = this.scoreMin();
    const max = this.scoreMax();
    return this.jobs().filter((j) => {
      if (kind !== 'all' && jobKindName(j.kind) !== kind) return false;
      if (min !== null && j.score < min) return false;
      if (max !== null && j.score > max) return false;

      return true;
    });
  });

  assignedRows = computed(() => collapseClusters(this.filteredJobs()));
  pendingRows = computed(() => collapseClusters(this.pendingJobs()));

  rowSubject({ job, subJobs }: BoardRow<DispatchedJobSummary | PendingJobSummary>): string {
    if (!job.cluster_job) return jobSubjectLabel(job.subject, job.exec_job);
    const exec = job.exec_job ? { ...job.exec_job, nodes: 1 } : job.exec_job;
    return `${jobSubjectLabel(job.subject, exec)} (cluster, ${this.subJobLabel(subJobs)})`;
  }

  subJobLabel(count: number): string {
    return count === 1 ? '1 sub job' : `${count} sub jobs`;
  }

  waitingFor(p: PendingJobSummary): WaitingExplanation | null {
    if (!p.waiting_reason || !p.project_name) return null;
    return explainWaiting(p.waiting_reason, p.project_name, waitingAccessFrom(p.permissions ?? [], false, this.superuser()));
  }

  pendingKey(p: PendingJobSummary): string {
    return p.cluster_job ?? p.evaluation_id + (p.build_id ?? '') + (p.exec_job?.id ?? '');
  }

  /// Recent decisions flattened to one row per candidate (incl. rejected and
  /// negative scores), honoring the type/score filters above.
  decisionRows = computed<DecisionRow[]>(() => {
    const kind = this.kindFilter();
    const min = this.scoreMin();
    const max = this.scoreMax();
    const rows: DecisionRow[] = [];
    for (const d of this.decisions()) {
      for (const c of d.candidates) {
        if (kind !== 'all' && jobKindName(c.kind) !== kind) continue;
        if (min !== null && c.score < min) continue;
        if (max !== null && c.score > max) continue;
        rows.push({
          id: c.id,
          at: d.at,
          worker: d.worker_name ?? d.worker_id,
          kind: c.kind,
          subject: jobSubjectLabel(c.subject, c.exec_job),
          score: c.score,
          won: c.won,
        });
      }
    }

    return rows;
  });

  ngOnInit(): void {
    this.load();
  }

  retry(): void {
    this.first.restart();
    this.load();
  }

  private load(): void {
    this.loadAssigned();
    if (this.view() === 'pending') this.loadPending();
    if (this.showDecisions()) this.loadDecisions();
  }

  setView(v: 'assigned' | 'pending'): void {
    this.view.set(v);
    this.first.refresh();
    if (v === 'pending') this.loadPending();
  }

  setScoreScope(scope: ScoreScope): void {
    this.scoreScope.set(scope);
    this.first.refresh();
    if (this.showDecisions()) this.loadDecisions();
  }

  private loadDecisions(): void {
    this.board.getAssignmentDecisions().pipe(this.first.track()).subscribe((d) => this.decisions.set(d));
  }

  private loadAssigned(): void {
    this.board.getAssignedJobs().pipe(this.first.track()).subscribe((r) => {
      this.jobs.set(r.jobs);
      this.otherRunning.set(r.other_running);
    });
  }

  private restoreState(): void {
    try {
      const raw = sessionStorage.getItem(BoardLiveJobsComponent.STATE_KEY);
      if (!raw) return;
      const s = JSON.parse(raw);
      if (s.view) this.view.set(s.view);
      if (s.kindFilter) this.kindFilter.set(s.kindFilter);
      this.scoreMin.set(s.scoreMin ?? null);
      this.scoreMax.set(s.scoreMax ?? null);
      if (s.scoreScope) this.scoreScope.set(s.scoreScope);
    } catch {
      /* ignore malformed state */
    }
  }

  private loadPending(): void {
    this.board.getPendingJobs().pipe(this.first.track()).subscribe((r) => {
      this.pendingJobs.set(r.jobs);
      this.otherPending.set(r.other_pending);
    });
  }

  inspect(j: DispatchedJobSummary): void {
    this.router.navigateByUrl(links.boardJobOrCluster(j.id, j.cluster_job) ?? links.boardJob(j.id));
  }

  inspectDecision(r: DecisionRow): void {
    this.router.navigateByUrl(links.boardJob(r.id));
  }
}
