/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Router, RouterModule, UrlTree } from '@angular/router';
import { BoardService } from '@core/services/board.service';
import { LiveStatusComponent } from '@shared/ui';
import { AuthService } from '@core/services/auth.service';
import { ButtonComponent, LoadMoreComponent, LoadStateComponent, TabSwitchComponent, TableComponent } from '@gradient/ui/ui';
import { firstLoad } from '../first-load';
import { BoardProjectTaskLinksComponent } from '../project-task-links.component';
import { subscribeToBoardSocketWithReload } from '../board-socket';
import { workerDisplayNameOrNotSharedLabel } from '../worker-label';
import { schedulerScoreDecimalPipeDigitsFormat } from '../scoring-rule-label';
import { BoardRow, JobKindName, collapseClusters, evaluationJobCommitLabel, jobKindName, jobSubjectLabel } from '../job-kind';
import { DispatchedJobSummary, PendingJobSummary, AssignDecisionView } from '@core/models';
import { links, oneOf, queryParamSignal } from '@shared/routing';
import { type WaitingExplanation, explainWaiting, waitingAccessFrom } from '@shared/evaluation';
import { plural, RelativeTimePipe, ServerTimestampToLocalDateTimePipe, serverTime } from '@shared/text';

const boardTableLoadMoreRowCount = 50;

type KindFilter = 'all' | JobKindName;
type ScoreScope = 'current' | 'all';
type JobListView = 'assigned' | 'pending';

function createScoreFilterQueryParamSignal(name: string) {
  const param = queryParamSignal<string>(name, '', (raw) => (raw.trim() !== '' && Number.isFinite(Number(raw)) ? raw : null));
  return Object.assign(computed(() => (param() === '' ? null : Number(param()))), {
    set: (value: number | null) => param.set(value === null || !Number.isFinite(value) ? '' : String(value)),
  });
}

function jobCountLine(shown: number, total: number, hidden: number, state: 'running' | 'pending'): string {
  const noun = `${state} job`;
  const count =
    total === 0
      ? `No ${noun}s${hidden > 0 ? ' you can see' : ''}`
      : shown === total
        ? plural(total, noun)
        : `${shown} of ${plural(total, noun)}`;
  return hidden > 0 ? `${count} (${hidden} on projects you can't view)` : count;
}

interface DecisionRow {
  id: string;
  at: string;
  worker: string;
  kind: number;
  subject: string;
  project?: string | null;
  projectDisplayName?: string | null;
  task?: string | null;
  taskDisplayName?: string | null;
  evaluation: string;
  score: number;
  won: boolean;
}

@Component({
  selector: 'app-board-live-jobs',
  standalone: true,
  imports: [RelativeTimePipe, CommonModule, ServerTimestampToLocalDateTimePipe, FormsModule, RouterModule, ButtonComponent, LoadMoreComponent, TableComponent, TabSwitchComponent, LoadStateComponent, LiveStatusComponent, BoardProjectTaskLinksComponent],
  template: `
    <div class="view-toggle">
      <gr-tab-switch ariaLabel="Job list" [options]="assignedAndPendingJobListTabOptions" [ngModel]="view()" (ngModelChange)="setView($event)" />
      @if (connected() !== null) { <gr-live-status [connected]="!!connected()" /> }
    </div>

    @if (view() === 'assigned') {
      <gr-load-state [loading]="assignedJobsPanelFirstLoad.loading()" [error]="assignedJobsPanelFirstLoad.error()" [empty]="!assignedJobsPanelFirstLoad.received()"
                     loadingMessage="Loading jobs..." errorTitle="Failed to load jobs" (retry)="retryAssigned()">
        <div class="banner">{{ runningJobsCount() }}</div>

        <div class="filters">
          @if (superuser()) {
            <label>Scores
              <select [ngModel]="scoreScope()" (ngModelChange)="setScoreScope($event)">
                <option value="current">assigned</option>
                <option value="all">incl. passed over</option>
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
          @if (superuser()) {
            <label>Score min
              <input type="number" step="0.1" [ngModel]="scoreMin()" (ngModelChange)="scoreMin.set($event)" placeholder="-" />
            </label>
            <label>Score max
              <input type="number" step="0.1" [ngModel]="scoreMax()" (ngModelChange)="scoreMax.set($event)" placeholder="-" />
            </label>
          }
        </div>

        @if (!showDecisions()) {
          @if (assignedRows().length || assignedEmptyText()) {
          <gr-table class="jobs">
            <thead>
              <tr><th>Kind</th><th>Project / task</th><th>Worker</th><th>Derivation / Evaluation / Exec</th>@if (superuser()) {<th>Score</th>}<th>Assigned</th><th><span class="sr-only">Details</span></th></tr>
            </thead>
            <tbody>
              @for (row of assignedRows(); track row.job.id) {
                @let j = row.job;
                @let detail = assignedJobDetailPageUrl(j);
                <tr class="clickable" (click)="router.navigateByUrl(detail)">
                  @let subject = rowSubject(row);
                  <td>{{ jobKindName(j.kind) }}</td>
                  <td class="origin">
                    <app-board-project-task-links [project]="j.project_name" [projectDisplayName]="j.project_display_name"
                                                  [task]="j.task" [taskDisplayName]="j.task_display_name" [evaluation]="j.evaluation_id" />
                  </td>
                  <td class="mono worker">{{ j.cluster_job ? subJobLabel(row.subJobs) : workerDisplayNameOrNotSharedLabel(j.worker_name) }}</td>
                  <td class="mono subject" [title]="subject">
                    <a [routerLink]="detail" (click)="$event.stopPropagation()">{{ subject }}</a>
                  </td>
                  @if (superuser()) {
                    <td>{{ j.score | number: scoreDigits }}</td>
                  }
                  <td class="age" [title]="j.dispatched_at | serverDate">{{ j.dispatched_at | relativeTime }}</td>
                  <td>›</td>
                </tr>
              } @empty {
                <tr><td [attr.colspan]="superuser() ? 7 : 6" class="muted">{{ assignedEmptyText() }}</td></tr>
              }
            </tbody>
          </gr-table>
          }
        } @else {
          <gr-table class="jobs">
            <thead>
              <tr><th>Decision</th><th>Kind</th><th>Project / task</th><th>Worker</th><th>Derivation / Evaluation / Exec</th><th>Score</th><th>Decided</th><th><span class="sr-only">Details</span></th></tr>
            </thead>
            <tbody>
              @for (r of visibleDecisionRows(); track r.id + r.at) {
                <tr [class.negative]="r.score < 0" class="clickable" (click)="router.navigateByUrl(links.boardJob(r.id))">
                  <td>{{ r.won ? 'Won' : 'Passed over' }}</td>
                  <td>{{ jobKindName(r.kind) }}</td>
                  <td class="origin">
                    <app-board-project-task-links [project]="r.project" [projectDisplayName]="r.projectDisplayName"
                                                  [task]="r.task" [taskDisplayName]="r.taskDisplayName" [evaluation]="r.evaluation" />
                  </td>
                  <td class="mono worker">{{ r.worker }}</td>
                  <td class="mono subject" [title]="r.subject">
                    <a [routerLink]="links.boardJob(r.id)" (click)="$event.stopPropagation()">{{ r.subject }}</a>
                  </td>
                  <td>{{ r.score | number: scoreDigits }}</td>
                  <td class="age" [title]="r.at | serverDate">{{ r.at | relativeTime }}</td>
                  <td>›</td>
                </tr>
              } @empty {
                <tr><td colspan="8" class="muted">No recent decisions.</td></tr>
              }
            </tbody>
          </gr-table>
          <gr-load-more [shown]="visibleDecisionRows().length" [total]="decisionRows().length"
                        (more)="visibleDecisionRowCount.set(visibleDecisionRowCount() + boardTableLoadMoreRowCount)" />
        }
      </gr-load-state>
    } @else {
      <gr-load-state [loading]="pendingJobsFirstLoad.loading()" [error]="pendingJobsFirstLoad.error()" [empty]="!pendingJobsFirstLoad.received()"
                     loadingMessage="Loading jobs..." errorTitle="Failed to load jobs" (retry)="retryPending()">
        <div class="banner">{{ pendingJobsCount() }}</div>
        @if (pendingRows().length) {
        <gr-table class="jobs pending">
          <thead><tr><th>Kind</th><th>Project / task</th><th>Derivation / Evaluation / Exec</th><th>Waiting for</th><th>Age</th></tr></thead>
          <tbody>
            @for (row of visiblePendingRows(); track pendingKey(row.job)) {
              @let p = row.job;
              @let subject = rowSubject(row);
              <tr class="clickable" (click)="router.navigateByUrl(links.boardPendingJob(p))">
                <td class="kind">{{ jobKindName(p.kind) }}</td>
                <td class="origin">
                  <app-board-project-task-links [project]="p.project_name" [projectDisplayName]="p.project_display_name"
                                                [task]="p.task" [taskDisplayName]="p.task_display_name" [evaluation]="p.evaluation_id" />
                </td>
                <td class="mono subject"><a [routerLink]="links.boardPendingJob(p)" [title]="subject" (click)="$event.stopPropagation()">{{ subject }}</a></td>
                <td class="waiting">
                  @if (waitingFor(p); as wait) {
                    <div class="wait">
                      <span [title]="wait.detail">{{ wait.title }}</span>
                      @if (wait.action; as action) {
                        @if (action.kind === 'link') {
                          <a grButton size="small" severity="secondary" [label]="action.label" [icon]="action.icon" [routerLink]="action.link" (click)="$event.stopPropagation()"></a>
                        }
                      } @else if (wait.hint) {
                        <span class="muted hint">{{ wait.hint }}</span>
                      }
                    </div>
                  } @else if (p.dependency_count > 0) {
                    <span class="muted">Waiting for {{ plural(p.dependency_count, 'dependency', 'dependencies') }}</span>
                  } @else {
                    <span class="muted">Queued, waiting for the scheduler</span>
                  }
                </td>
                <td class="age" [title]="p.queued_at | serverDate">{{ p.queued_at | relativeTime }}</td>
              </tr>
            }
          </tbody>
        </gr-table>
        <gr-load-more [shown]="visiblePendingRows().length" [total]="pendingRows().length"
                      (more)="visiblePendingJobRowCount.set(visiblePendingJobRowCount() + boardTableLoadMoreRowCount)" />
        }
      </gr-load-state>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './live-jobs.component.scss',
})
export class BoardLiveJobsComponent implements OnInit {
  protected readonly links = links;

  private board = inject(BoardService);
  protected router = inject(Router);
  private auth = inject(AuthService);
  protected assignedJobsPanelFirstLoad = firstLoad();
  protected pendingJobsFirstLoad = firstLoad();
  protected connected = subscribeToBoardSocketWithReload(() => this.load());
  protected readonly jobKindName = jobKindName;
  protected readonly workerDisplayNameOrNotSharedLabel = workerDisplayNameOrNotSharedLabel;
  protected readonly scoreDigits = schedulerScoreDecimalPipeDigitsFormat;
  protected readonly jobSubjectLabel = jobSubjectLabel;
  protected readonly plural = plural;
  protected readonly boardTableLoadMoreRowCount = boardTableLoadMoreRowCount;

  jobs = signal<DispatchedJobSummary[]>([]);
  otherRunning = signal(0);

  readonly assignedAndPendingJobListTabOptions = [
    { label: 'Assigned', value: 'assigned' },
    { label: 'Pending', value: 'pending' },
  ] as const;
  view = queryParamSignal<JobListView>('view', 'assigned', oneOf(this.assignedAndPendingJobListTabOptions.map((o) => o.value)));
  pendingJobs = signal<PendingJobSummary[]>([]);
  otherPending = signal(0);

  kindFilter = queryParamSignal<KindFilter>('type', 'all', oneOf<KindFilter>(['all', 'eval', 'build', 'exec']));
  scoreMin = createScoreFilterQueryParamSignal('min');
  scoreMax = createScoreFilterQueryParamSignal('max');

  scoreScope = queryParamSignal<ScoreScope>('scores', 'current', oneOf<ScoreScope>(['current', 'all']));
  decisions = signal<AssignDecisionView[]>([]);
  protected superuser = computed(() => this.auth.user()?.superuser === true);
  showDecisions = computed(() => this.superuser() && this.scoreScope() === 'all');

  scoreFilterMinimumExceedsMaximum = computed(() => {
    const min = this.scoreMin();
    const max = this.scoreMax();
    return this.superuser() && min !== null && max !== null && min > max;
  });

  filteredJobs = computed(() => {
    const kind = this.kindFilter();
    const min = this.superuser() ? this.scoreMin() : null;
    const max = this.superuser() ? this.scoreMax() : null;
    return this.jobs().filter((j) => {
      if (kind !== 'all' && jobKindName(j.kind) !== kind) return false;
      if (min !== null && j.score != null && j.score < min) return false;
      if (max !== null && j.score != null && j.score > max) return false;

      return true;
    });
  });

  assignedRows = computed(() => collapseClusters(this.filteredJobs()));

  runningJobsFilterIsSet = computed(() =>
    this.kindFilter() !== 'all' || (this.superuser() && (this.scoreMin() !== null || this.scoreMax() !== null)));

  runningJobsCount = computed(() =>
    jobCountLine(this.assignedRows().length, collapseClusters(this.jobs()).length, this.otherRunning(), 'running'));

  assignedEmptyText = computed(() => {
    if (this.scoreFilterMinimumExceedsMaximum()) return 'Score min is above Score max.';
    return this.runningJobsFilterIsSet() && this.jobs().length ? 'No matching running jobs.' : '';
  });
  pendingRows = computed(() =>
    collapseClusters([...this.pendingJobs()].sort((a, b) => serverTime(a.queued_at).getTime() - serverTime(b.queued_at).getTime())));
  visiblePendingJobRowCount = signal(boardTableLoadMoreRowCount);
  visiblePendingRows = computed(() => this.pendingRows().slice(0, this.visiblePendingJobRowCount()));
  pendingJobsCount = computed(() =>
    jobCountLine(this.pendingRows().length, this.pendingRows().length, this.otherPending(), 'pending'));

  rowSubject({ job, subJobs }: BoardRow<DispatchedJobSummary | PendingJobSummary>): string {
    const subject = evaluationJobCommitLabel(job) ?? job.subject;
    if (!job.cluster_job) return jobSubjectLabel(subject, job.exec_job);
    const exec = job.exec_job ? { ...job.exec_job, nodes: 1 } : job.exec_job;
    return `${jobSubjectLabel(subject, exec)} (cluster, ${this.subJobLabel(subJobs)})`;
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

  /// Recent decisions flattened to one row per candidate, including passed-over candidates and negative scores. The type and score filters above apply.
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
          project: c.project_name,
          projectDisplayName: c.project_display_name,
          task: c.task,
          taskDisplayName: c.task_display_name,
          evaluation: c.evaluation_id,
          score: c.score,
          won: c.won,
        });
      }
    }

    return rows;
  });
  visibleDecisionRowCount = signal(boardTableLoadMoreRowCount);
  visibleDecisionRows = computed(() => this.decisionRows().slice(0, this.visibleDecisionRowCount()));

  ngOnInit(): void {
    this.load();
  }

  retryAssigned(): void {
    this.assignedJobsPanelFirstLoad.restart();
    this.loadAssignedPanel();
  }

  retryPending(): void {
    this.pendingJobsFirstLoad.restart();
    this.loadPending();
  }

  private load(): void {
    this.loadAssignedPanel();
    if (this.view() === 'pending') this.loadPending();
  }

  setView(v: JobListView): void {
    this.view.set(v);
    if (v === 'pending') {
      this.pendingJobsFirstLoad.refresh();
      this.loadPending();
    }
  }

  setScoreScope(scope: ScoreScope): void {
    this.scoreScope.set(scope);
    this.assignedJobsPanelFirstLoad.refresh();
    if (this.showDecisions()) this.loadDecisions();
  }

  private loadAssignedPanel(): void {
    this.board.getAssignedJobs().pipe(this.assignedJobsPanelFirstLoad.track()).subscribe((r) => {
      this.jobs.set(r.jobs);
      this.otherRunning.set(r.other_running);
    });
    if (this.showDecisions()) this.loadDecisions();
  }

  private loadDecisions(): void {
    this.board.getAssignmentDecisions().pipe(this.assignedJobsPanelFirstLoad.track()).subscribe((d) => this.decisions.set(d));
  }

  private loadPending(): void {
    this.board.getPendingJobs().pipe(this.pendingJobsFirstLoad.track()).subscribe((r) => {
      this.pendingJobs.set(r.jobs);
      this.otherPending.set(r.other_pending);
    });
  }

  assignedJobDetailPageUrl(j: DispatchedJobSummary): UrlTree {
    return links.boardJobOrCluster(j.id, j.cluster_job) ?? links.boardJob(j.id);
  }
}
