/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, ElementRef, HostListener, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule, DOCUMENT } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { finalize, interval, Observable, Subscription } from 'rxjs';
import { auditTime, filter, share } from 'rxjs/operators';
import { LiveEvent, LiveService } from '@core/services/live.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService, ReportOptions } from '@core/services/tasks.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import {
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  EmptyStateComponent,
  IconComponent,
  InViewDirective,
  MenuComponent,
  MenuItem,
  MessageService,
  SkeletonComponent,
  ToastComponent,
  TooltipDirective,
} from '@gradient/ui/ui';
import { EvalStatusBadgeComponent, inputFetchRow, SegmentedBarComponent, StarButtonComponent, StatusIconComponent } from '@shared/ui';
import { AccessService, WritableDirective } from '@shared/access';
import { injectTaskAccess, injectTaskAccessData } from '@core/resolvers/inject-access';
import { groupEntryPoints } from './entry-point-groups';
import { StarTarget, TaskDetail, EvaluationSummary, EvaluationProgress, EvaluationStatus, EntryPointSummary, FailedAttributeSummary, BuildStatusCounts, WalkMode } from '@core/models';
import { buildDuration, commitLabel, entryPointPhase, evaluationDuration, evaluationPhase, evaluationProgressText, evaluationTitle, formatEvaluationDuration, inputFetchPhase, isPendingBuildStatus, isRunningEvaluationStatus, phaseProgress } from '@shared/evaluation';

@Component({
  selector: 'app-task-detail',
  standalone: true,
  imports: [
    CommonModule, FormsModule, RouterModule, ButtonComponent, CheckboxComponent, DialogComponent, MenuComponent, TooltipDirective,
    SkeletonComponent, EmptyStateComponent, WritableDirective,
    SegmentedBarComponent, EvalStatusBadgeComponent,
    IconComponent, InViewDirective, StatusIconComponent, ToastComponent, StarButtonComponent,
  ],
  providers: [MessageService],
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
  private evaluationsService = inject(EvaluationsService);
  private messageService = inject(MessageService);
  private accessService = inject(AccessService);
  private live = inject(LiveService);
  private document = inject(DOCUMENT);
  private stars = inject(StarsService);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  private resolved = injectTaskAccessData();
  task = signal<TaskDetail | null>(null);
  entryPoints = signal<EntryPointSummary[]>([]);
  entryPointsTotal = signal(0);
  failedAttributes = signal<FailedAttributeSummary[]>([]);
  entryPointsLoading = signal(false);
  // Mirrors the server's own page size and its hard cap, so "show more" pages with
  // an offset instead of asking for a limit the server would clamp.
  private static readonly ENTRY_POINTS_PAGE = 25;
  private static readonly ENTRY_POINTS_PAGE_MAX = 500;
  private entryPointsAppending = false;
  selectedId = signal<string | null>(null);
  starting = signal(false);
  private evaluationsBeforeStart: Set<string> | null = null;
  starred = signal(false);
  errorMessage = signal<string | null>(null);
  abortTarget = signal<string | null>(null);
  aborting = signal(false);
  tick = signal(Date.now());

  projectName = '';
  projectDisplayName = signal<string | null>(null);
  taskName = '';
  starTarget: StarTarget = { kind: 'task', project: '', task: '' };

  private liveSub?: Subscription;
  private querySub?: Subscription;
  private tickSubscription?: Subscription;

  // Content signatures + a throttle so a running evaluation's rapid live pings
  // don't re-render the cards or re-run the expensive entry-point query.
  private taskSig = '';
  private entryPointsEvalId?: string;
  private entryPointsSig = '';
  private lastEntryPointsFetch = 0;
  private readonly ENTRY_POINTS_LIVE_INTERVAL_MS = 4000;

  private readonly OLDER_PAGE = 10;
  private olderEvaluations = signal<EvaluationSummary[]>([]);
  private loadingOlder = false;
  hasOlderEvaluations = signal(true);
  evaluations = computed(() => {
    const newest = this.task()?.last_evaluations ?? [];
    const shown = new Set(newest.map(e => e.id));
    return [...newest, ...this.olderEvaluations().filter(e => !shown.has(e.id))];
  });
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

  private activity = signal<Record<string, EvaluationProgress>>({});
  selectedProgress = computed<EvaluationProgress | null>(() => {
    const sel = this.selected();
    return sel ? phaseProgress(sel.status, this.activity()[sel.id], sel.progress) : null;
  });
  selectedFetchRows = computed(() => {
    const p = this.selectedProgress();
    return p?.kind === 'fetching' ? p.inputs.map(inputFetchRow) : [];
  });
  selectedProgressText = computed(() => evaluationProgressText(this.selectedProgress()));

  latestEvaluation = computed<EvaluationSummary | null>(() => this.evaluations()[0] ?? null);
  evaluationInProgress = computed(() => {
    const e = this.latestEvaluation();
    return !!e && isRunningEvaluationStatus(e.status);
  });

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.starTarget = { kind: 'task', project: this.projectName, task: this.taskName };
    this.stars.starred(this.starTarget).subscribe((starred) => this.starred.set(starred));
    this.querySub = this.route.queryParamMap.subscribe((q) => this.followEvalParam(q.get('eval')));
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => this.projectDisplayName.set(this.projectName),
    });
    const resolved = this.resolved()?.task;
    if (resolved) this.applyTask(resolved, false);
    this.loadTaskData(!!resolved);
    this.startLiveUpdates();
    this.tickSubscription = interval(1000).subscribe(() => this.tick.set(Date.now()));
  }

  ngOnDestroy(): void {
    this.cancelNewEvaluationSelect();
    this.liveSub?.unsubscribe();
    this.querySub?.unsubscribe();
    this.tickSubscription?.unsubscribe();
  }

  private followEvalParam(id: string | null): void {
    if (!id || id === this.selectedId()) return;
    if (this.task()) this.showEvaluation(id);
    else this.selectedId.set(id);
  }

  loadOlderEvaluations(): void {
    const oldest = this.evaluations().at(-1);
    if (this.loadingOlder || !this.hasOlderEvaluations() || !oldest) return;
    this.loadingOlder = true;
    this.tasksService.getEvaluations(this.projectName, this.taskName, this.OLDER_PAGE, oldest.id)
      .pipe(finalize(() => (this.loadingOlder = false)))
      .subscribe({
        next: page => {
          this.olderEvaluations.update(loaded => [...loaded, ...page]);
          if (page.length < this.OLDER_PAGE) this.hasOlderEvaluations.set(false);
        },
        error: () => this.hasOlderEvaluations.set(false),
      });
  }

  loadTaskData(live = false): void {
    this.tasksService.getTask(this.projectName, this.taskName).subscribe({
      next: (task) => this.applyTask(task, live),
      error: (error) => console.error('Failed to load task:', error),
    });
  }

  private applyTask(task: TaskDetail, live: boolean): void {
    const sig = this.taskSignature(task);
    if (sig !== this.taskSig) {
      this.taskSig = sig;
      this.task.set(task);
    }
    if (this.starting() && task.last_evaluations.some(e => this.isRunning(e.status))) {
      this.starting.set(false);
    }
    const started = task.last_evaluations.find(e => this.evaluationsBeforeStart && !this.evaluationsBeforeStart.has(e.id));
    if (started) {
      this.select(started);
      return;
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
  }

  /// Fields whose change should re-render the header / eval strip / panel.
  private taskSignature(p: TaskDetail): string {
    const evals = (p.last_evaluations ?? [])
      .map(e => `${e.id}:${e.status}:${e.prioritized}:${e.errors}:${e.warnings}:${e.updated_at}:${JSON.stringify(e.builds)}`)
      .join('|');
    return [p.active, p.can_edit, p.can_trigger, p.display_name, p.description, p.repository,
      p.wildcard, p.last_check_at, JSON.stringify(p.queue), evals].join('§');
  }

  truncate(value: string, max = 42): string {
    return value.length > max ? value.slice(0, max) + '…' : value;
  }

  select(evaluation: EvaluationSummary): void {
    this.cancelNewEvaluationSelect();
    this.showEvaluation(evaluation.id);
    // Keep the selection in the URL so navigating away and back restores it.
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { eval: evaluation.id },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  private showEvaluation(id: string): void {
    if (this.selectedId() !== id) {
      // Drop the previous evaluation's packages immediately so the panel shows a
      // loading state, not stale data, during the (slow) entry-point fetch.
      this.entryPoints.set([]);
      this.entryPointsTotal.set(0);
      this.failedAttributes.set([]);
      this.entryPointsEvalId = undefined;
      this.entryPointsSig = '';
    }
    this.selectedId.set(id);
    this.loadEntryPoints(id);
  }

  private entryPointSignature(eps: EntryPointSummary[]): string {
    return eps.map(e => `${e.id}:${e.build_status}:${e.prioritized}:${e.build_time_ms}:${e.has_artefacts}:${JSON.stringify(e.deps)}`).join('|');
  }

  private loadEntryPoints(evaluationId?: string): void {
    if (!evaluationId) {
      this.entryPoints.set([]);
      this.entryPointsTotal.set(0);
      this.failedAttributes.set([]);
      this.entryPointsEvalId = undefined;
      this.entryPointsSig = '';
      this.entryPointsLoading.set(false);
      return;
    }
    this.lastEntryPointsFetch = Date.now();
    const switching = evaluationId !== this.entryPointsEvalId;
    if (switching) this.entryPointsLoading.set(true);
    // A refresh re-reads the first page only, never the whole scrolled window:
    // the server walks one dependency closure per entry point it returns, so
    // re-reading a 500-row window every 4 s costs twenty times what the visible
    // head costs. Rows past the head keep their last values and are spliced
    // behind the refreshed page.
    const limit = TaskDetailComponent.ENTRY_POINTS_PAGE;
    this.tasksService.getEntryPoints(this.projectName, this.taskName, evaluationId, limit, 0).subscribe({
      next: (page) => {
        // Drop out-of-order responses: only apply the fetch for the still-selected
        // evaluation, so a slow earlier request can't clobber a newer selection.
        if (this.selectedId() !== evaluationId) return;
        this.entryPointsLoading.set(false);
        this.entryPointsTotal.set(page.total);
        if (JSON.stringify(page.failed_attributes) !== JSON.stringify(this.failedAttributes())) {
          this.failedAttributes.set(page.failed_attributes);
        }
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

  /// Merge a refreshed page with the rows already shown. The page is the server's
  /// rows at offset 0, so every row it does not hold ranks after it and keeps its
  /// display order behind it; matching on id rather than on position is what lets
  /// entry points newly added to the page fall out of the tail without a gap.
  private spliceEntryPoints(page: EntryPointSummary[], shown: EntryPointSummary[]): EntryPointSummary[] {
    const inPage = new Set(page.map(e => e.id));

    return [...page, ...shown.filter(e => !inPage.has(e.id))];
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

  startEvaluation(walk: WalkMode = 'pruned'): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.selectNextNewEvaluation();
    this.tasksService.startEvaluation(this.projectName, this.taskName, walk).subscribe({
      next: () => this.loadTaskData(),
      error: (error) => {
        this.cancelNewEvaluationSelect();
        this.errorMessage.set(error?.message || 'Failed to start evaluation.');
        this.starting.set(false);
      },
    });
  }

  private selectNextNewEvaluation(): void {
    this.evaluationsBeforeStart = new Set(this.evaluations().map(e => e.id));
    this.document.addEventListener('pointerdown', this.cancelNewEvaluationSelect, true);
    this.document.addEventListener('keydown', this.cancelNewEvaluationSelect, true);
  }

  private cancelNewEvaluationSelect = (): void => {
    this.evaluationsBeforeStart = null;
    this.document.removeEventListener('pointerdown', this.cancelNewEvaluationSelect, true);
    this.document.removeEventListener('keydown', this.cancelNewEvaluationSelect, true);
  };

  restartFailedBuilds(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.tasksService.restartFailedBuilds(this.projectName, this.taskName).subscribe({
      next: () => this.loadTaskData(),
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
        this.loadTaskData();
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
    const frames = this.live
      .connect<LiveEvent>(`/tasks/${this.projectName}/${this.taskName}/live`)
      .pipe(share());
    this.liveSub = frames
      .pipe(filter(e => e.event === 'evaluation.activity'))
      .subscribe(e => {
        const { evaluation_id, progress } = e.content;
        if (evaluation_id && progress) this.activity.update(all => ({ ...this.runningActivity(all), [evaluation_id]: progress }));
      });
    this.liveSub.add(
      frames
        .pipe(filter(e => e.event !== 'evaluation.activity'), auditTime(500))
        .subscribe(() => this.loadTaskData(true)),
    );
  }

  /// Left/right arrows step through the evaluation strip.
  @HostListener('document:keydown', ['$event'])
  onKeydown(e: KeyboardEvent): void {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
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

  private runningActivity(all: Record<string, EvaluationProgress>): Record<string, EvaluationProgress> {
    const running = new Set(this.evaluations().filter(e => this.isRunning(e.status)).map(e => e.id));
    return Object.fromEntries(Object.entries(all).filter(([id]) => running.has(id)));
  }

  evalDuration(evaluation: EvaluationSummary): string {
    return formatEvaluationDuration(evaluationDuration(evaluation, this.tick()));
  }

  pkgDuration(ep: EntryPointSummary): string {
    const ms = buildDuration({ status: ep.build_status, ...ep }, this.tick());
    return ms == null ? '' : formatEvaluationDuration(ms);
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

  entryPointGroups = computed(() => groupEntryPoints(this.entryPoints(), this.failedAttributes()));

  protected readonly evaluationPhase = evaluationPhase;
  protected readonly entryPointPhase = entryPointPhase;
  protected readonly inputFetchPhase = inputFetchPhase;

  pkgMenuModel = signal<MenuItem[]>([]);

  // Report generation is authenticated server-side, so an anonymous visitor
  // browsing a public task is not offered an action that can only 403.
  panelMenuModel = computed<MenuItem[]>(() => {
    const selected = this.selected();
    const job = selected?.dispatched_job;
    return [
      { label: 'Logs', icon: 'article', disabled: !selected,
        routerLink: selected
          ? ['/project', this.projectName, 'log', selected.id]
          : undefined },
      { label: 'Show job', icon: 'work', disabled: !job,
        routerLink: job ? ['/board', 'jobs', job] : undefined },
      { label: 'Metrics', icon: 'show_chart',
        routerLink: ['/project', this.projectName, 'task', this.taskName, 'metrics'] },
      ...(selected && this.canRestartFailed(selected)
        ? [{ label: 'Restart failed builds', icon: 'refresh', disabled: this.starting(),
             command: () => this.restartFailedBuilds() }]
        : []),
      ...(selected && this.canPrioritizeEvaluation(selected)
        ? [{ label: 'Prioritize', icon: 'keyboard_double_arrow_up',
             command: () => this.prioritize(this.evaluationsService.prioritizeEvaluation(selected.id), 'Evaluation') }]
        : []),
      ...(this.authService.isAuthenticated() && this.triggerAccess().canEdit
        ? [{ label: 'Full rewalk', icon: 'account_tree',
             disabled: this.starting() || this.evaluationInProgress(),
             command: () => this.startEvaluation('full') }]
        : []),
      ...(this.authService.isAuthenticated()
        ? [{ label: 'Diagnostic report', icon: 'bug_report', disabled: !selected,
             command: () => this.reportDialogOpen.set(true) }]
        : []),
    ];
  });

  private canPrioritize(): boolean {
    return this.authService.isAuthenticated() && this.triggerAccess().canEdit;
  }

  private canPrioritizeEvaluation(evaluation: EvaluationSummary): boolean {
    return this.canPrioritize() && !evaluation.prioritized && this.isRunning(evaluation.status);
  }

  private canPrioritizeEntryPoint(ep: EntryPointSummary): boolean {
    return this.canPrioritize() && !ep.prioritized && isPendingBuildStatus(ep.build_status);
  }

  private prioritize(request: Observable<string>, target: 'Evaluation' | 'Build'): void {
    request.subscribe({
      next: () => {
        this.messageService.add({ severity: 'success', summary: `${target} prioritized` });
        this.loadTaskData();
      },
      error: (error: Error) => this.errorMessage.set(error?.message || `Failed to prioritize ${target.toLowerCase()}.`),
    });
  }

  /// The server restarts the task's newest evaluation, so only that one offers it.
  private canRestartFailed(evaluation: EvaluationSummary): boolean {
    return this.triggerAccess().canEdit
      && evaluation.id === this.evaluations()[0]?.id
      && !this.isRunning(evaluation.status)
      && evaluation.builds.failed + evaluation.builds.aborted > 0;
  }

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
    const built = ep.build_status === 'Completed' || ep.build_status === 'Substituted';
    const canArtefacts = built && ep.has_artefacts;
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
        label: 'View closure', icon: 'hub', disabled: !built,
        routerLink: built ? ['/project', this.projectName, 'closure', 'build', ep.build_id] : undefined,
      },
      {
        label: 'Entry-point metrics', icon: 'show_chart',
        routerLink: ['/project', this.projectName, 'task', this.taskName, 'entry-point-metrics'],
        queryParams: { eval: ep.eval },
      },
      ...(this.canPrioritizeEntryPoint(ep)
        ? [{ label: 'Prioritize', icon: 'keyboard_double_arrow_up',
             command: () => this.prioritize(this.evaluationsService.prioritizeBuild(ep.build_id), 'Build') }]
        : []),
    ];
  }

  openPkgMenu(event: Event, ep: EntryPointSummary, evalId: string, menu: { toggle: (e: Event) => void }): void {
    event.stopPropagation();
    this.pkgMenuModel.set(this.buildPkgMenu(ep, evalId));
    menu.toggle(event);
  }

  openPkgContextMenu(event: MouseEvent, ep: EntryPointSummary, evalId: string, menu: { openAt: (e: MouseEvent) => void }): void {
    this.pkgMenuModel.set(this.buildPkgMenu(ep, evalId));
    menu.openAt(event);
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
      // Settled work with no result: it is not pending, so it draws nothing.
      case 'Skipped': break;
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
