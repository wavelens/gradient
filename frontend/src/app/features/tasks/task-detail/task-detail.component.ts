/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, ElementRef, HostListener, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { auditTime } from 'rxjs/operators';
import { LiveService } from '@core/services/live.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { EvalStatusBadgeComponent } from '@shared/components/eval-status-badge/eval-status-badge.component';
import { AccessService, WritableDirective } from '@shared/access';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import { TaskDetail, EvaluationSummary, EvaluationStatus, EntryPointSummary, BuildStatus, BuildStatusCounts } from '@core/models';
import { commitLabel, evaluationTitle, formatEvaluationDuration, isRunningEvaluationStatus, parseUtcTimestamp } from '@shared/evaluation';
import { SegmentedBarComponent } from './segmented-bar/segmented-bar.component';
import { ButtonComponent, DialogComponent, MenuComponent, MenuItem, TooltipDirective } from '@shared/ui';

@Component({
  selector: 'app-task-detail',
  standalone: true,
  imports: [
    CommonModule, RouterModule, ButtonComponent, DialogComponent, MenuComponent, TooltipDirective,
    LoadingSpinnerComponent, EmptyStateComponent, WritableDirective,
    SegmentedBarComponent, EvalStatusBadgeComponent,
  ],
  templateUrl: './task-detail.component.html',
  styleUrls: [
    './task-detail.component.scss',
    './task-detail.evaluations.scss',
    './task-detail.packages.scss',
  ],
})
export class TaskDetailComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private host = inject(ElementRef);
  protected authService = inject(AuthService);
  private projectsService = inject(ProjectsService);
  private tasksService = inject(TasksService);
  private accessService = inject(AccessService);
  private live = inject(LiveService);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  loading = signal(true);
  task = signal<TaskDetail | null>(null);
  entryPoints = signal<EntryPointSummary[]>([]);
  entryPointsLoading = signal(false);
  selectedId = signal<string | null>(null);
  starting = signal(false);
  errorMessage = signal<string | null>(null);
  abortTarget = signal<string | null>(null);
  tick = signal(Date.now());

  projectName = '';
  projectDisplayName = signal('');
  taskName = '';

  private liveSub?: Subscription;
  private tickSubscription?: Subscription;

  // Content signatures + a throttle so a running evaluation's rapid live pings
  // don't re-render the cards or re-run the expensive entry-point query.
  private taskSig = '';
  private entryPointsEvalId?: string;
  private entryPointsSig = '';
  private lastEntryPointsFetch = 0;
  private readonly ENTRY_POINTS_LIVE_INTERVAL_MS = 4000;

  evaluations = computed(() => this.task()?.last_evaluations ?? []);
  selected = computed<EvaluationSummary | null>(() => {
    const id = this.selectedId();
    const list = this.evaluations();
    return list.find(e => e.id === id) ?? list[0] ?? null;
  });
  // Single-item keyed list: @for tracked by id recreates the panel DOM on
  // selection change, retriggering its CSS enter animation.
  selectedList = computed<EvaluationSummary[]>(() => {
    const s = this.selected();
    return s ? [s] : [];
  });

  latestEvaluation = computed<EvaluationSummary | null>(() => this.evaluations()[0] ?? null);
  evaluationInProgress = computed(() => {
    const e = this.latestEvaluation();
    return !!e && isRunningEvaluationStatus(e.status);
  });

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.selectedId.set(this.route.snapshot.queryParamMap.get('eval'));
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.loadTaskData();
    this.startLiveUpdates();
    this.tickSubscription = interval(1000).subscribe(() => this.tick.set(Date.now()));
  }

  ngOnDestroy(): void {
    this.liveSub?.unsubscribe();
    this.tickSubscription?.unsubscribe();
  }

  loadTaskData(showLoading = true, live = false): void {
    if (showLoading) this.loading.set(true);
    this.tasksService.getTask(this.projectName, this.taskName).subscribe({
      next: (task) => {
        const sig = this.taskSignature(task);
        if (sig !== this.taskSig) {
          this.taskSig = sig;
          this.task.set(task);
        }
        if (showLoading) this.loading.set(false);
        if (this.starting() && task.last_evaluations.some(e => this.isRunning(e.status))) {
          this.starting.set(false);
        }
        if (!this.selectedId() && task.last_evaluations.length) {
          this.selectedId.set(task.last_evaluations[0].id);
        }
        // The entry-point query (dep-closure CTE) is expensive; on live pings
        // throttle it so a running evaluation's rapid status stream doesn't
        // hammer the backend. The cheap summary above keeps headline counts live.
        if (!live || Date.now() - this.lastEntryPointsFetch >= this.ENTRY_POINTS_LIVE_INTERVAL_MS) {
          this.loadEntryPoints(this.selected()?.id);
        }
      },
      error: (error) => {
        console.error('Failed to load task:', error);
        if (showLoading) this.loading.set(false);
      },
    });
  }

  /// Fields whose change should re-render the header / eval strip / panel.
  private taskSignature(p: TaskDetail): string {
    const evals = (p.last_evaluations ?? [])
      .map(e => `${e.id}:${e.status}:${e.errors}:${e.warnings}:${e.updated_at}:${JSON.stringify(e.builds)}`)
      .join('|');
    return [p.active, p.can_edit, p.can_trigger, p.display_name, p.description, p.repository,
      p.wildcard, p.last_check_at, JSON.stringify(p.queue), evals].join('§');
  }

  truncate(value: string, max = 42): string {
    return value.length > max ? value.slice(0, max) + '…' : value;
  }

  select(evaluation: EvaluationSummary): void {
    if (this.selectedId() !== evaluation.id) {
      // Drop the previous evaluation's packages immediately so the panel shows a
      // loading state, not stale data, during the (slow) entry-point fetch.
      this.entryPoints.set([]);
      this.entryPointsEvalId = undefined;
      this.entryPointsSig = '';
    }
    this.selectedId.set(evaluation.id);
    this.loadEntryPoints(evaluation.id);
    // Keep the selection in the URL so navigating away and back restores it.
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { eval: evaluation.id },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  private loadEntryPoints(evaluationId?: string): void {
    if (!evaluationId) {
      this.entryPoints.set([]);
      this.entryPointsEvalId = undefined;
      this.entryPointsSig = '';
      this.entryPointsLoading.set(false);
      return;
    }
    this.lastEntryPointsFetch = Date.now();
    if (evaluationId !== this.entryPointsEvalId) this.entryPointsLoading.set(true);
    this.tasksService.getEntryPoints(this.projectName, this.taskName, evaluationId).subscribe({
      next: (eps) => {
        // Drop out-of-order responses: only apply the fetch for the still-selected
        // evaluation, so a slow earlier request can't clobber a newer selection.
        if (this.selectedId() !== evaluationId) return;
        this.entryPointsLoading.set(false);
        const sorted = [...eps].sort((a, b) =>
          this.getDerivationName(a.derivation_path).localeCompare(this.getDerivationName(b.derivation_path)));
        // Skip the re-render (and its enter animation) when nothing changed.
        const sig = sorted.map(e => `${e.id}:${e.build_status}:${e.build_time_ms}:${e.has_artefacts}:${JSON.stringify(e.deps)}`).join('|');
        if (evaluationId === this.entryPointsEvalId && sig === this.entryPointsSig) return;
        this.entryPointsEvalId = evaluationId;
        this.entryPointsSig = sig;
        this.entryPoints.set(sorted);
      },
      error: (error) => {
        if (this.selectedId() === evaluationId) this.entryPointsLoading.set(false);
        console.error('Failed to load entry points:', error);
      },
    });
  }

  startEvaluation(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.tasksService.startEvaluation(this.projectName, this.taskName).subscribe({
      next: () => this.loadTaskData(false),
      error: (error) => {
        this.errorMessage.set(error?.message || 'Failed to start evaluation.');
        this.starting.set(false);
      },
    });
  }

  restartFailedBuilds(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.tasksService.restartFailedBuilds(this.projectName, this.taskName).subscribe({
      next: () => this.loadTaskData(false),
      error: (error) => {
        this.errorMessage.set(error?.message || 'Failed to restart failed builds.');
        this.starting.set(false);
      },
    });
  }

  confirmAbort(): void {
    const id = this.abortTarget();
    this.abortTarget.set(null);
    if (!id) return;
    this.tasksService.abortEvaluation(this.projectName, this.taskName, id).subscribe({
      next: () => this.loadTaskData(false),
      error: (error) => console.error('Failed to abort evaluation:', error),
    });
  }

  dismissError(): void { this.errorMessage.set(null); }

  private startLiveUpdates(): void {
    this.liveSub = this.live
      .connect(`/tasks/${this.projectName}/${this.taskName}/live`)
      .pipe(auditTime(500))
      .subscribe(() => this.loadTaskData(false, true));
  }

  /// Left/right arrows step through the evaluation strip.
  @HostListener('document:keydown', ['$event'])
  onKeydown(e: KeyboardEvent): void {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)) return;
    const list = this.evaluations();
    if (!list.length) return;
    const cur = this.selected();
    const idx = cur ? list.findIndex(x => x.id === cur.id) : 0;
    const next = e.key === 'ArrowLeft' ? idx - 1 : idx + 1;
    if (next < 0 || next >= list.length) return;
    e.preventDefault();
    this.select(list[next]);
    requestAnimationFrame(() =>
      this.host.nativeElement.querySelector('.eval-card.selected')
        ?.scrollIntoView({ inline: 'nearest', block: 'nearest', behavior: 'smooth' }));
  }

  isRunning(status: EvaluationStatus): boolean { return isRunningEvaluationStatus(status); }

  evalDuration(evaluation: EvaluationSummary): string {
    const start = parseUtcTimestamp(evaluation.created_at);
    const end = this.isRunning(evaluation.status) ? this.tick() : parseUtcTimestamp(evaluation.updated_at);
    return formatEvaluationDuration(end - start);
  }

  formatDurationMs(ms: number | null): string {
    if (ms == null) return '';
    return formatEvaluationDuration(ms);
  }

  readonly commitLabel = commitLabel;

  evalTitle(e: EvaluationSummary): string {
    return evaluationTitle(e);
  }

  triggerLabel(e: EvaluationSummary): string {
    if (e.triggered_by) return e.triggered_by;
    switch (e.trigger?.type) {
      case 'polling': return 'Polling';
      case 'reporter_push': return 'Push';
      case 'reporter_pull_request': return e.pr_number ? `PR #${e.pr_number}` : 'PR';
      case 'time': return 'Schedule';
      default: return 'Manual';
    }
  }

  getDerivationName(path: string): string {
    const parts = path.split('/').pop() ?? path;
    const match = parts.match(/^[a-z0-9]+-(.+?)(?:\.drv)?$/);
    return match ? match[1] : parts;
  }

  statusClass(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'ok';
      case 'Failed': return 'err';
      case 'Aborted': return 'muted';
      case 'Waiting': return 'warn';
      default: return 'run';
    }
  }

  statusIcon(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'check_circle';
      case 'Failed': return 'error';
      case 'Aborted': return 'cancel';
      case 'Waiting': return 'pause_circle';
      case 'Fetching': return 'cloud_download';
      case 'Queued': return 'schedule';
      default: return 'sync';
    }
  }

  buildStatusClass(status: BuildStatus): string {
    switch (status) {
      case 'Completed': case 'Substituted': return 'ok';
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': return 'err';
      case 'Aborted': case 'DependencyFailed': return 'muted';
      case 'Building': return 'run';
      default: return 'warn';
    }
  }

  buildStatusIcon(status: BuildStatus): string {
    switch (status) {
      case 'Completed': case 'Substituted': return 'check_circle';
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': return 'error';
      case 'Aborted': case 'DependencyFailed': return 'cancel';
      case 'Building': return 'sync';
      default: return 'schedule';
    }
  }

  pkgMenuModel = signal<MenuItem[]>([]);

  panelMenuModel = computed<MenuItem[]>(() => [
    { label: 'Metrics', icon: 'show_chart',
      routerLink: ['/project', this.projectName, 'task', this.taskName, 'metrics'] },
  ]);

  private buildPkgMenu(ep: EntryPointSummary, evalId: string): MenuItem[] {
    const canArtefacts = (ep.build_status === 'Completed' || ep.build_status === 'Substituted') && ep.has_artefacts;
    return [
      {
        label: 'Artefacts', icon: 'download', disabled: !canArtefacts,
        routerLink: canArtefacts ? ['/project', this.projectName, 'artefacts', ep.build_id] : undefined,
        queryParams: canArtefacts ? { task: this.taskName } : undefined,
      },
      {
        label: 'Dependency graph', icon: 'account_tree',
        routerLink: ['/project', this.projectName, 'graph', ep.build_id],
        queryParams: { evalId: evalId, task: this.taskName },
      },
      {
        label: 'Entry-point metrics', icon: 'show_chart',
        routerLink: ['/project', this.projectName, 'task', this.taskName, 'entry-point-metrics'],
        queryParams: { eval: ep.eval },
      },
    ];
  }

  openPkgMenu(event: Event, ep: EntryPointSummary, evalId: string, menu: { toggle: (e: Event) => void }): void {
    event.stopPropagation();
    this.pkgMenuModel.set(this.buildPkgMenu(ep, evalId));
    menu.toggle(event);
  }

  doneCount(c: BuildStatusCounts): number {
    return c.completed + c.failed + c.substituted + c.aborted;
  }

  totalCount(c: BuildStatusCounts): number {
    return this.doneCount(c) + c.building + c.queued;
  }

  /// Dep-closure counts plus the entry point's own build, so a package with
  /// few or no deps still shows its own progress in the bar.
  barCounts(ep: EntryPointSummary): BuildStatusCounts {
    const c = { ...ep.deps };
    switch (ep.build_status) {
      case 'Completed': c.completed++; break;
      case 'Substituted': c.substituted++; break;
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': c.failed++; break;
      case 'Aborted': case 'DependencyFailed': c.aborted++; break;
      case 'Building': c.building++; break;
      default: c.queued++;
    }
    return c;
  }
}
