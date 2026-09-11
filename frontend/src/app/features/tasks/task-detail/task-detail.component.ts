/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, ElementRef, HostListener, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { auditTime } from 'rxjs/operators';
import { LiveService } from '@core/services/live.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService, ReportOptions } from '@core/services/tasks.service';
import { ButtonComponent, CheckboxComponent, DialogComponent, EmptyStateComponent, EvalStatusBadgeComponent, IconComponent, LoadingSpinnerComponent, MenuComponent, MenuItem, TooltipDirective } from '@shared/ui';
import { AccessService, WritableDirective } from '@shared/access';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import { TaskDetail, EvaluationSummary, EvaluationStatus, EntryPointSummary, BuildStatus, BuildStatusCounts } from '@core/models';
import { commitLabel, evaluationTitle, formatEvaluationDuration, isRunningEvaluationStatus, parseUtcTimestamp } from '@shared/evaluation';
import { SegmentedBarComponent } from './segmented-bar/segmented-bar.component';

@Component({
  selector: 'app-task-detail',
  standalone: true,
  imports: [
    CommonModule, FormsModule, RouterModule, ButtonComponent, CheckboxComponent, DialogComponent, MenuComponent, TooltipDirective,
    LoadingSpinnerComponent, EmptyStateComponent, WritableDirective,
    SegmentedBarComponent, EvalStatusBadgeComponent,
    IconComponent,
  ],
  templateUrl: './task-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
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
  entryPointsTotal = signal(0);
  entryPointsLoading = signal(false);
  // Mirrors the server's own page size and its hard cap, so "show more" pages with
  // an offset instead of asking for a limit the server would clamp.
  private static readonly ENTRY_POINTS_PAGE = 100;
  private static readonly ENTRY_POINTS_PAGE_MAX = 500;
  private entryPointsAppending = false;
  selectedId = signal<string | null>(null);
  starting = signal(false);
  errorMessage = signal<string | null>(null);
  abortTarget = signal<string | null>(null);
  aborting = signal(false);
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
        // The entry-point page walks the graph for stale histograms; on live pings
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
      this.entryPointsTotal.set(0);
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

  private entryPointSignature(eps: EntryPointSummary[]): string {
    return eps.map(e => `${e.id}:${e.build_status}:${e.build_time_ms}:${e.has_artefacts}:${JSON.stringify(e.deps)}`).join('|');
  }

  private loadEntryPoints(evaluationId?: string): void {
    if (!evaluationId) {
      this.entryPoints.set([]);
      this.entryPointsTotal.set(0);
      this.entryPointsEvalId = undefined;
      this.entryPointsSig = '';
      this.entryPointsLoading.set(false);
      return;
    }
    this.lastEntryPointsFetch = Date.now();
    const switching = evaluationId !== this.entryPointsEvalId;
    if (switching) this.entryPointsLoading.set(true);
    // A refresh re-reads the window it already shows, never below one page (an
    // evaluation still ingesting entry points must keep filling in) and never
    // above the server's maximum, which keeps a live poll at one request.
    const limit = switching
      ? TaskDetailComponent.ENTRY_POINTS_PAGE
      : Math.min(
          Math.max(this.entryPoints().length, TaskDetailComponent.ENTRY_POINTS_PAGE),
          TaskDetailComponent.ENTRY_POINTS_PAGE_MAX,
        );
    this.tasksService.getEntryPoints(this.projectName, this.taskName, evaluationId, limit, 0).subscribe({
      next: (page) => {
        // Drop out-of-order responses: only apply the fetch for the still-selected
        // evaluation, so a slow earlier request can't clobber a newer selection.
        if (this.selectedId() !== evaluationId) return;
        this.entryPointsLoading.set(false);
        this.entryPointsTotal.set(page.total);
        const next = this.spliceEntryPoints(
          page.entry_points,
          switching ? [] : this.entryPoints(),
        );
        // Skip the re-render (and its enter animation) when nothing changed.
        const sig = this.entryPointSignature(next);
        if (evaluationId === this.entryPointsEvalId && sig === this.entryPointsSig) return;
        this.entryPointsEvalId = evaluationId;
        this.entryPointsSig = sig;
        this.entryPoints.set(next);
      },
      error: (error) => {
        if (this.selectedId() === evaluationId) this.entryPointsLoading.set(false);
        console.error('Failed to load entry points:', error);
      },
    });
  }

  /// Merge a refreshed window with the rows already shown. The window is the
  /// first N of the server's order, so anything it does not hold sorts after it
  /// and simply follows. A tail that no longer lines up - new entry points landed
  /// inside a window the user had already paged past - is dropped rather than
  /// rendered with a hole in it, and "show more" fetches it again.
  private spliceEntryPoints(window: EntryPointSummary[], shown: EntryPointSummary[]): EntryPointSummary[] {
    const inWindow = new Set(window.map(e => e.id));
    const tail = shown.filter(e => !inWindow.has(e.id));
    const last = window.at(-1);
    if (!last || tail.length === 0) return [...window, ...tail];
    const follows = tail[0].eval > last.eval || (tail[0].eval === last.eval && tail[0].id > last.id);
    return follows ? [...window, ...tail] : [...window];
  }

  loadMoreEntryPoints(): void {
    const evaluationId = this.selected()?.id;
    if (!evaluationId || this.entryPointsAppending) return;
    this.entryPointsAppending = true;
    const offset = this.entryPoints().length;
    this.tasksService.getEntryPoints(this.projectName, this.taskName, evaluationId, TaskDetailComponent.ENTRY_POINTS_PAGE, offset).subscribe({
      next: (page) => {
        this.entryPointsAppending = false;
        if (this.selectedId() !== evaluationId) return;
        this.entryPointsTotal.set(page.total);
        const shown = this.entryPoints();
        const seen = new Set(shown.map(e => e.id));
        const next = [...shown, ...page.entry_points.filter(e => !seen.has(e.id))];
        this.entryPointsEvalId = evaluationId;
        this.entryPointsSig = this.entryPointSignature(next);
        this.entryPoints.set(next);
      },
      error: (error) => {
        this.entryPointsAppending = false;
        console.error('Failed to load more entry points:', error);
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
    if (!id || this.aborting()) return;
    this.aborting.set(true);
    this.tasksService.abortEvaluation(this.projectName, this.taskName, id).subscribe({
      next: () => {
        this.aborting.set(false);
        this.abortTarget.set(null);
        this.loadTaskData(false);
      },
      error: (error: Error) => {
        this.aborting.set(false);
        this.abortTarget.set(null);
        this.errorMessage.set(error?.message || 'Failed to abort evaluation.');
      },
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

  /// Last segment of the Nix attribute path, which is what the server orders the
  /// page by, so the visible label and the visible order are the same field.
  attrLabel(attr: string): string {
    // Segments are dot-separated outside quotes, so `pkgs."x.y"` ends at `x.y`.
    const last = (attr.match(/"[^"]*"|[^."]+/g) ?? []).at(-1) ?? attr;
    return last.replace(/^"|"$/g, '') || attr;
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

  // Report generation is authenticated server-side, so an anonymous visitor
  // browsing a public task is not offered an action that can only 403.
  panelMenuModel = computed<MenuItem[]>(() => {
    const selected = this.selected();
    return [
      { label: 'Logs', icon: 'article', disabled: !selected,
        routerLink: selected
          ? ['/project', this.projectName, 'log', selected.id]
          : undefined },
      { label: 'Metrics', icon: 'show_chart',
        routerLink: ['/project', this.projectName, 'task', this.taskName, 'metrics'] },
      ...(this.authService.isAuthenticated()
        ? [{ label: 'Diagnostic report', icon: 'bug_report', disabled: !selected,
             command: () => this.reportDialogOpen.set(true) }]
        : []),
    ];
  });

  reportDialogOpen = signal(false);
  reportBusy = signal(false);
  reportOptions = signal<ReportOptions>({
    include_identities: false,
    include_packages: true,
    include_logs: false,
    include_instance: true,
  });

  // The instance section needs ManageWorkers, which the task access payload
  // does not carry, so the box stays enabled and the server's 403 explains
  // itself. Disabling it up front needs `can_manage_workers` on AccessState.

  setReportOption(key: keyof ReportOptions, value: boolean): void {
    this.reportOptions.update(o => ({ ...o, [key]: value }));
  }

  generateReport(): void {
    const evaluation = this.selected();
    if (!evaluation || this.reportBusy()) return;

    this.reportBusy.set(true);
    this.tasksService.downloadReport(evaluation.id, this.reportOptions()).subscribe({
      next: response => {
        this.reportBusy.set(false);
        this.reportDialogOpen.set(false);
        if (response.body) {
          this.saveReport(response.body, filenameFromDisposition(
            response.headers.get('content-disposition'),
            `gradient-report-${evaluation.id.split('-')[0]}.db`,
          ));
        }
      },
      error: (e: Error) => {
        this.reportBusy.set(false);
        this.errorMessage.set(e.message);
      },
    });
  }

  private saveReport(blob: Blob, filename: string): void {
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = filename;
    anchor.click();
    URL.revokeObjectURL(url);
  }

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

/// Prefer the server's filename, since it names the evaluation and the date.
export function filenameFromDisposition(header: string | null, fallback: string): string {
  const match = header?.match(/filename="([^"]+)"/);
  return match?.[1] ?? fallback;
}
