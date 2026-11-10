/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, ElementRef, HostListener, Injector, afterNextRender, afterRenderEffect, computed, inject, signal, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { catchError, finalize, map, Observable, of, Subject, Subscription, switchMap } from 'rxjs';
import { auditTime, debounceTime, distinctUntilChanged, filter, share, tap } from 'rxjs/operators';
import { takeUntilDestroyed, toObservable, toSignal } from '@angular/core/rxjs-interop';
import { LiveEvent, LiveService, createResyncOnLiveSocketReconnectCallback } from '@core/services/live.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TasksService, ReportOptions } from '@core/services/tasks.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import { RepositoryFetchBannerComponent } from '@shared/git';
import {
  BadgeComponent,
  BreadcrumbComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  EmptyStateComponent,
  IconComponent,
  InViewDirective,
  LoadStateComponent,
  MenuComponent,
  MenuItem,
  MessageBannerComponent,
  SkeletonComponent,
  TooltipDirective,
} from '@gradient/ui/ui';
import { EvalStatusBadgeComponent, inputFetchRows, LiveStatusComponent, SegmentedBarComponent, StarButtonComponent, StatusIconComponent, ThunkProgressComponent, WaitingNoticeComponent } from '@shared/ui';
import { saveResponse } from '@shared/download';
import { AccessService, WritableDirective, canOpenTaskSettings } from '@shared/access';
import { injectTaskAccess, injectTaskAccessData } from '@core/resolvers/inject-access';
import { groupEntryPoints } from './entry-point-groups';
import { firstLine } from '@features/evaluations/evaluation-summary/summary-text';
import { ExecJobListComponent } from '@features/exec-jobs/exec-job-list/exec-job-list.component';
import { links } from '@shared/routing';
import { removeAnsiEscapeSequences } from '@shared/log';
import { CurrentTimeSignal, formatDuration, monthDayClockTime, plural, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import {
  StarTarget,
  TaskDetailsResponse,
  EntryPointSummary,
  FailedAttributeSummary,
  BuildStatusCounts,
  EvaluationSummary,
  EvaluationProgress,
  WalkMode,
  EvaluationStatus,
  ExecJobSummary,
} from '@core/models';
import { abortBuildConfirm, buildStatusNameFromStatusCode, abortedWithoutBuildsTitle, abortEvaluationConfirm, buildAbortTargetLine, evaluationAbortTarget, buildDurationTextForEvaluation, buildStatusToShowInEvaluation, canAbortBuild, canPrioritizeBuild, canRetryBuild, commitLabel, commitWebUrl, getArtefactsMenuItemDisabledReason, getClosureMenuItemDisabledReason, getSbomDownloadMenuItemDisabledReason, showJobMenuItemDisabledReason, entryPointPhaseWithExecJob, phaseInFinishedEvaluation, evaluationDuration, evaluationPhase, evaluationTitle, explainWaiting, getEvaluationTriggerLabel, inputFetchPhase, isEvaluationNotStartedStatus, isRunningEvaluationStatus, phaseProgress, removeNixErrorInternalsFromEvaluationMessage, buildTally, getBuildCountTextPieces, repositoryWebUrl, type BuildTally, splitRepository, thunkProgress, waitingAccessFrom, type StatusPhase } from '@shared/evaluation';

const NO_EVALUATION_SELECTED = 'No evaluation selected.';

@Component({
  selector: 'app-task-detail',
  standalone: true,
  imports: [
    CommonModule, ServerTimestampToLocalDateTimePipe, FormsModule, RouterModule, BadgeComponent, BreadcrumbComponent, ButtonComponent, CheckboxComponent, DialogComponent, MenuComponent, TooltipDirective,
    SkeletonComponent, EmptyStateComponent, LoadStateComponent, MessageBannerComponent, WritableDirective,
    SegmentedBarComponent, ThunkProgressComponent, EvalStatusBadgeComponent,
    IconComponent, InViewDirective, StatusIconComponent, StarButtonComponent,
    ExecJobListComponent, WaitingNoticeComponent,
    RepositoryFetchBannerComponent, LiveStatusComponent,
  ],
  templateUrl: './task-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: [
    '../../../shared/evaluation/entry-rows.scss',
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
  private crumbs = inject(BreadcrumbsService);
  private tasksService = inject(TasksService);
  private evaluationsService = inject(EvaluationsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private accessService = inject(AccessService);
  private live = inject(LiveService);
  private stars = inject(StarsService);
  private injector = inject(Injector);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));
  waitingAccess = computed(() =>
    waitingAccessFrom(this.resolved()?.task.permissions ?? [], this.canTrigger(), this.isSuperuser()),
  );

  private resolved = injectTaskAccessData();
  canOpenSettings = computed(() => canOpenTaskSettings(this.resolved()?.task));
  canIncludeInstance = computed(() => this.resolved()?.task.permissions.includes('manageWorkers') ?? false);
  task = signal<TaskDetailsResponse | null>(null);
  entryPoints = signal<EntryPointSummary[]>([]);
  selectedEvaluationExecJobsLastReportedByList = signal<ExecJobSummary[]>([]);
  execJobStatusByEntryPointAttributePath = computed(() => new Map(this.selectedEvaluationExecJobsLastReportedByList().map((job) => [job.entry_point, job.status])));
  entryPointsTotal = signal(0);
  failedAttributes = signal<FailedAttributeSummary[]>([]);
  entryPointsLoading = signal(false);
  entryPointsError = signal<unknown>(null);
  moreEntryPointsError = signal<unknown>(null);
  // Mirrors the server's own page size and its hard cap, so "show more" pages with
  // an offset instead of asking for a limit the server would clamp.
  private static readonly ENTRY_POINTS_PAGE = 25;
  private static readonly SEARCH_DEBOUNCE_MS = 200;
  protected readonly staggeredRows = TaskDetailComponent.ENTRY_POINTS_PAGE;
  private barCountsByEntryPoint = new WeakMap<EntryPointSummary, BuildStatusCounts>();
  private entryPointsAppending = false;
  selectedId = signal<string | null>(null);
  starting = signal(false);
  starred = signal(false);
  aborting = signal(false);
  liveConnectionOpen = signal<boolean | null>(null);
  private tick = inject(CurrentTimeSignal).currentTimeMilliseconds;

  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.task(this.projectName, this.taskName));
  starTarget: StarTarget = { kind: 'task', project: '', task: '' };

  private liveSub?: Subscription;
  private querySub?: Subscription;

  // Content signatures + a throttle so a running evaluation's rapid live pings
  // don't re-render the cards or re-run the expensive entry-point query.
  private taskSig = '';
  private entryPointsEvalId?: string;
  private entryPointsSig = '';
  private lastEntryPointsFetch = 0;
  private lastFetchedEntryPointListEvaluationIdAndStatusKey = '';
  private entryPointsReloadTimer?: ReturnType<typeof setTimeout>;
  private readonly ENTRY_POINTS_LIVE_INTERVAL_MS = 4000;
  // Live reload, start and abort request the task even with another request open. Only responses newer than the last shown response take effect. A late "Building" response cannot overwrite a newer "Failed" response. A never-ending stream of live events cannot starve the page, unlike switchMap.
  private sentGetTaskRequestCount = 0;
  private newestShownTaskRequestNumber = 0;

  private readonly OLDER_PAGE = 10;
  private olderEvaluations = signal<EvaluationSummary[]>([]);
  private loadingOlder = false;
  hasOlderEvaluations = signal(true);
  olderEvaluationsLoadError = signal<string | null>(null);
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
    return p?.kind === 'fetching' ? inputFetchRows(p.inputs) : [];
  });
  selectedThunkRow = computed(() => thunkProgress(this.selectedProgress(), this.selected()?.expected_thunks));
  private failedEvaluationId = computed(() => {
    const selected = this.selected();
    return selected?.status === 'Failed' ? selected.id : null;
  });
  selectedEvaluationErrorFirstLine = toSignal(toObservable(this.failedEvaluationId).pipe(
    switchMap((id) => (id ? this.evaluationsService.getEvaluation(id).pipe(
      map((evaluation) => (evaluation.error ? firstLine(evaluation.error) : null)),
      catchError(() => of(null)),
    ) : of(null))),
  ), { initialValue: null });

  private requestedEvaluationIdMissingFromStrip = computed(() => {
    const id = this.selectedId();
    if (!id || !this.task()) return null;
    return this.evaluations().some(e => e.id === id) ? null : id;
  });
  requestedEvaluationNotFound = toSignal(toObservable(this.requestedEvaluationIdMissingFromStrip).pipe(
    distinctUntilChanged(),
    switchMap((id) => (id ? this.evaluationsService.getEvaluation(id).pipe(
      map((evaluation) => evaluation.project_name !== this.projectName || evaluation.task_name !== this.taskName),
      catchError(() => of(true)),
    ) : of(false))),
  ), { initialValue: false });

  latestEvaluation = computed<EvaluationSummary | null>(() => this.evaluations()[0] ?? null);
  evaluationInProgress = computed(() => {
    const e = this.latestEvaluation();
    return !!e && isRunningEvaluationStatus(e.status);
  });

  latestEvaluationNotStarted = computed(() => {
    const e = this.latestEvaluation();
    return !!e && isEvaluationNotStartedStatus(e.status);
  });

  startDisabledReason = computed(() => {
    const latest = this.latestEvaluation();
    if (!latest || !this.latestEvaluationNotStarted()) return null;
    const waiting = latest.waiting_reason && explainWaiting(latest.waiting_reason, this.projectName, this.waitingAccess()).title;
    return `The latest evaluation has not started yet${waiting ? `: ${waiting}` : ''}.`;
  });

  protected getWaitingReasonTitleForEvaluationWithNonWaitingStatus(evaluation: EvaluationSummary): string | null {
    if (!evaluation.waiting_reason || evaluation.status === 'Waiting') return null;
    return explainWaiting(evaluation.waiting_reason, this.projectName, this.waitingAccess()).title;
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.starTarget = { kind: 'task', project: this.projectName, task: this.taskName };
    this.stars.starred(this.starTarget).subscribe((starred) => this.starred.set(starred));
    this.querySub = this.route.queryParamMap.subscribe((q) => this.followEvalParam(q.get('eval')));
    const resolved = this.resolved()?.task;
    if (resolved) this.applyTask(resolved, false);
    else this.loadTaskData();
    this.startLiveUpdates();
  }

  ngOnDestroy(): void {
    this.liveSub?.unsubscribe();
    this.querySub?.unsubscribe();
    clearTimeout(this.entryPointsReloadTimer);
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
    this.olderEvaluationsLoadError.set(null);
    this.tasksService.getEvaluations(this.projectName, this.taskName, this.OLDER_PAGE, oldest.id)
      .pipe(finalize(() => (this.loadingOlder = false)))
      .subscribe({
        next: page => {
          this.olderEvaluations.update(loaded => [...loaded, ...page]);
          if (page.length < this.OLDER_PAGE) this.hasOlderEvaluations.set(false);
        },
        error: (error) => this.olderEvaluationsLoadError.set(apiErrorMessage(error, 'no response from the server')),
      });
  }

  loadTaskData(live = false): void {
    this.requestTaskDetailsIfNoNewerRequestApplied().subscribe({
      next: (task) => this.applyTask(task, live),
      error: (error) => console.error('Failed to load task:', error),
    });
  }

  private requestTaskDetailsIfNoNewerRequestApplied(): Observable<TaskDetailsResponse> {
    const request = ++this.sentGetTaskRequestCount;
    return this.tasksService.getTask(this.projectName, this.taskName).pipe(
      filter(() => request > this.newestShownTaskRequestNumber),
      tap(() => (this.newestShownTaskRequestNumber = request)),
    );
  }

  private applyTask(task: TaskDetailsResponse, live: boolean): void {
    const sig = this.taskSignature(task);
    if (sig !== this.taskSig) {
      this.taskSig = sig;
      this.task.set(task);
      this.crumbs.rememberTask(this.projectName, this.taskName, task.display_name);
    }
    // The entry-point page walks the graph for stale histograms; on live pings
    // throttle it so a running evaluation's rapid status stream doesn't
    // hammer the backend. The cheap summary above keeps headline counts live.
    const shown = this.selected();
    const statusChanged = this.lastFetchedEntryPointListEvaluationIdAndStatusKey !== this.getEvaluationStatusKey(shown);
    const wait = live && !statusChanged ? this.lastEntryPointsFetch + this.ENTRY_POINTS_LIVE_INTERVAL_MS - Date.now() : 0;
    if (wait <= 0) {
      this.loadEntryPoints(shown?.id, statusChanged);
    } else {
      this.entryPointsReloadTimer ??= setTimeout(() => this.loadEntryPoints(this.selected()?.id), wait);
    }
  }

  private getEvaluationStatusKey(evaluation: EvaluationSummary | null | undefined): string {
    return evaluation ? `${evaluation.id}:${evaluation.status}` : '';
  }

  /// Fields whose change should re-render the header / eval strip / panel.
  private taskSignature(p: TaskDetailsResponse): string {
    const evals = (p.last_evaluations ?? [])
      .map(e => `${e.id}:${e.status}:${e.prioritized}:${e.errors}:${e.warnings}:${e.updated_at}:${JSON.stringify(e.builds)}:${JSON.stringify(e.waiting_reason)}`)
      .join('|');
    return [p.active, p.can_edit, p.can_trigger, p.display_name, p.description, p.repository,
      p.wildcard, p.last_check_at, evals].join('§');
  }

  select(evaluation: EvaluationSummary): void {
    this.selectEvaluation(evaluation.id);
  }

  private selectEvaluation(id: string): void {
    this.showEvaluation(id);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { eval: id },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  private showEvaluation(id: string): void {
    this.selectedId.set(id);
    this.crumbs.rememberEvaluation(this.projectName, this.taskName, id);
    this.loadEntryPoints(this.selected()?.id);
  }

  private entryPointSignature(eps: EntryPointSummary[]): string {
    return eps.map(e => `${e.id}:${e.build_status}:${e.prioritized}:${e.build_time_ms}:${e.has_artefacts}:${JSON.stringify(e.deps)}`).join('|');
  }

  private loadEntryPoints(evaluationId?: string, reloadAllLoadedRows = false): void {
    clearTimeout(this.entryPointsReloadTimer);
    this.entryPointsReloadTimer = undefined;
    const shownPackageListIsFromDifferentEvaluation = this.entryPointsEvalId !== undefined && evaluationId !== this.entryPointsEvalId;
    if (!evaluationId || shownPackageListIsFromDifferentEvaluation) {
      // Drop the previous evaluation's packages immediately so the panel shows a
      // loading state, not stale data, during the (slow) entry-point fetch.
      this.entryPoints.set([]);
      this.selectedEvaluationExecJobsLastReportedByList.set([]);
      this.entryPointsTotal.set(0);
      this.failedAttributes.set([]);
      this.entryPointsEvalId = undefined;
      this.entryPointsSig = '';
    }
    if (!evaluationId) {
      this.entryPointsLoading.set(false);
      return;
    }
    this.lastEntryPointsFetch = Date.now();
    this.lastFetchedEntryPointListEvaluationIdAndStatusKey = this.getEvaluationStatusKey(this.evaluations().find(e => e.id === evaluationId));
    const switching = evaluationId !== this.entryPointsEvalId;
    if (switching) {
      this.entryPointsLoading.set(true);
      this.entryPointsError.set(null);
      this.moreEntryPointsError.set(null);
    }
    // Live refreshes reload only the first page. Returned entry points add one dependency closure each for the server to walk. Reloads of a 500-row window every 4 seconds cost 20 times as much. Rows after the first page take build status from live events instead. Reload the entire loaded range once on a change in evaluation status.
    const limit = reloadAllLoadedRows && !switching
      ? Math.max(TaskDetailComponent.ENTRY_POINTS_PAGE, this.entryPoints().length)
      : TaskDetailComponent.ENTRY_POINTS_PAGE;
    const term = this.searchTerm();
    this.tasksService.getEntryPoints(this.projectName, this.taskName, evaluationId, limit, 0, term).subscribe({
      next: (page) => {
        // Drop out-of-order responses: only apply the fetch for the still-selected
        // evaluation and search term, so a slow earlier request can't clobber a newer one.
        if (this.selected()?.id !== evaluationId || this.searchTerm() !== term) return;
        this.entryPointsLoading.set(false);
        this.entryPointsError.set(null);
        this.entryPointsTotal.set(page.total);
        if (!term) this.listSpansPages.set(page.total > TaskDetailComponent.ENTRY_POINTS_PAGE);
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
      error: (error: unknown) => {
        if (this.selected()?.id !== evaluationId || this.searchTerm() !== term) return;
        this.entryPointsLoading.set(false);
        this.entryPointsError.set(error);
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
    this.moreEntryPointsError.set(null);
    const offset = this.entryPoints().length;
    const term = this.searchTerm();
    this.tasksService.getEntryPoints(this.projectName, this.taskName, evaluationId, TaskDetailComponent.ENTRY_POINTS_PAGE, offset, term).subscribe({
      next: (page) => {
        this.entryPointsAppending = false;
        if (this.selected()?.id !== evaluationId || this.searchTerm() !== term) return;
        this.entryPointsTotal.set(page.total);
        const shown = this.entryPoints();
        const seen = new Set(shown.map(e => e.id));
        const next = [...shown, ...page.entry_points.filter(e => !seen.has(e.id))];
        this.entryPointsEvalId = evaluationId;
        this.entryPointsSig = this.entryPointSignature(next);
        this.entryPoints.set(next);
      },
      error: (error: unknown) => {
        this.entryPointsAppending = false;
        if (this.selected()?.id === evaluationId) this.moreEntryPointsError.set(error);
      },
    });
  }

  startEvaluation(walk: WalkMode = 'pruned'): void {
    if (this.starting()) return;
    this.starting.set(true);
    this.tasksService.startEvaluation(this.projectName, this.taskName, walk).pipe(
      switchMap((evaluation) => {
        this.selectEvaluation(evaluation);
        return this.requestTaskDetailsIfNoNewerRequestApplied().pipe(catchError(() => of(null)));
      }),
      finalize(() => this.starting.set(false)),
    ).subscribe({
      next: (task) => task && this.applyTask(task, true),
      error: (error: unknown) => this.notify.failure('Failed to start evaluation', error),
    });
  }

  private retryEvaluation(evaluation: EvaluationSummary): void {
    this.evaluationsService.retryEvaluation(evaluation.id).subscribe({
      next: (retried) => {
        this.notify.success('Evaluation retried');
        this.openNewEvaluation(retried);
      },
      error: (error: unknown) => this.notify.failure('Failed to retry evaluation', error),
    });
  }

  private openNewEvaluation(id: string): void {
    this.selectEvaluation(id);
    this.loadTaskData(true);
  }

  requestAbort(event: MouseEvent, evaluation: EvaluationSummary): void {
    if (event.ctrlKey || event.metaKey) {
      this.abortAtOnce(evaluation.id);
      return;
    }
    const target = evaluationAbortTarget(this.task()?.display_name || this.taskName, evaluation.commit);
    this.confirm.confirm(abortEvaluationConfirm(target, () => this.abortRequest(evaluation.id)))
      .subscribe(() => this.evaluationAborted());
  }

  private abortAtOnce(id: string): void {
    if (this.aborting()) return;
    this.aborting.set(true);
    this.abortRequest(id).subscribe({
      next: () => {
        this.aborting.set(false);
        this.evaluationAborted();
      },
      error: (error: unknown) => {
        this.aborting.set(false);
        this.notify.failure('Failed to abort evaluation', error);
      },
    });
  }

  private abortRequest(id: string): Observable<string> {
    return this.tasksService.abortEvaluation(this.projectName, this.taskName, id);
  }

  private evaluationAborted(): void {
    this.notify.success('Evaluation aborted');
    this.loadTaskData();
  }

  private startLiveUpdates(): void {
    const frames = this.live
      .connect<LiveEvent>(
        `/tasks/${this.projectName}/${this.taskName}/live`,
        createResyncOnLiveSocketReconnectCallback(() => this.loadTaskData(), (open) => this.liveConnectionOpen.set(open)),
      )
      .pipe(share());
    this.liveSub = frames
      .pipe(filter(e => e.event === 'evaluation.activity'))
      .subscribe(e => {
        const { evaluation_id, progress } = e.content;
        if (evaluation_id && progress) this.activity.update(all => ({ ...this.runningActivity(all), [evaluation_id]: progress }));
      });
    this.liveSub.add(
      frames.subscribe(({ content }) => {
        const status = buildStatusNameFromStatusCode(content.status);
        if (!status || content.evaluation_id !== this.entryPointsEvalId) return;
        const rows = this.entryPoints();
        const index = rows.findIndex(row => row.build_id === content.build_id);
        if (index >= 0 && rows[index].build_status !== status) this.entryPoints.set(rows.map((row, i) => (i === index ? { ...row, build_status: status } : row)));
      }),
    );
    this.liveSub.add(
      frames
        .pipe(filter(e => e.event !== 'evaluation.activity'), auditTime(500))
        .subscribe(() => this.loadTaskData(true)),
    );
  }

  /// Packages load a page at a time, so the browser find would miss the rest.
  openSearch(): void {
    this.searchOpened.set(true);
    afterNextRender(() => {
      const field = this.searchField()?.nativeElement;
      field?.focus();
      field?.select();
    }, { injector: this.injector });
  }

  search(term: string): void {
    this.typedTerms.next(term.trim());
  }

  closeSearch(): void {
    const field = this.searchField()?.nativeElement;
    if (field) field.value = '';
    this.searchOpened.set(false);
    this.typedTerms.next('');
    this.applySearch('');
  }

  closeEmptySearch(): void {
    if (!this.searchField()?.nativeElement.value.trim()) this.closeSearch();
  }

  reloadEntryPoints(): void {
    this.entryPointsEvalId = undefined;
    this.loadEntryPoints(this.selected()?.id);
  }

  private applySearch(term: string): void {
    if (term === this.searchTerm()) return;
    this.searchTerm.set(term);
    this.entryPointsEvalId = undefined;
    this.loadEntryPoints(this.selected()?.id);
  }

  @HostListener('document:keydown', ['$event'])
  openPackageSearchOnControlOrCommandFKeydownInEvaluationDetail(e: KeyboardEvent): void {
    if (!(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== 'f' || !this.selected()) return;
    if (!(e.target instanceof Element && e.target.closest('#evaluation-detail-panel'))) return;
    e.preventDefault();
    this.openSearch();
  }

  onEvaluationTabListKeydown(e: KeyboardEvent): void {
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    const list = this.evaluations();
    if (!list.length) return;
    const cur = this.selected();
    const idx = cur ? list.findIndex(x => x.id === cur.id) : 0;
    const next = ({ ArrowLeft: idx - 1, ArrowRight: idx + 1, Home: 0, End: list.length - 1 } as Record<string, number | undefined>)[e.key];
    if (next === undefined) return;
    if (next < 0 || next >= list.length || next === idx) return;
    e.preventDefault();
    this.select(list[next]);
    requestAnimationFrame(() => {
      const card = this.host.nativeElement.querySelector('.eval-card.selected') as HTMLElement | null;
      card?.focus({ preventScroll: true });
      card?.scrollIntoView({ inline: 'nearest', block: 'nearest', behavior: 'smooth' });
    });
  }

  private evaluationTabsScrollElement = viewChild<ElementRef<HTMLElement>>('evaluationTabsScrollElement');
  isEvaluationTabStripScrolledAwayFromStart = signal(false);
  hasHiddenOlderEvaluationTabs = signal(false);
  private readonly evaluationTabScrollArrowsAfterRenderEffect = afterRenderEffect(() => {
    this.evaluations();
    this.updateEvaluationTabScrollArrows();
  });

  @HostListener('window:resize')
  updateEvaluationTabScrollArrows(): void {
    const strip = this.evaluationTabsScrollElement()?.nativeElement;
    this.isEvaluationTabStripScrolledAwayFromStart.set(!!strip && strip.scrollLeft > 1);
    this.hasHiddenOlderEvaluationTabs.set(!!strip && strip.scrollLeft + strip.clientWidth < strip.scrollWidth - 1);
  }

  scrollEvaluationTabStripByDirection(direction: -1 | 1): void {
    const strip = this.evaluationTabsScrollElement()?.nativeElement;
    strip?.scrollBy({ left: direction * strip.clientWidth * 0.8, behavior: 'smooth' });
  }

  readonly monthDayClockTime = monthDayClockTime;

  isRunning(status: EvaluationStatus): boolean { return isRunningEvaluationStatus(status); }
  protected readonly links = links;

  private runningActivity(all: Record<string, EvaluationProgress>): Record<string, EvaluationProgress> {
    const running = new Set(this.evaluations().filter(e => this.isRunning(e.status)).map(e => e.id));
    return Object.fromEntries(Object.entries(all).filter(([id]) => running.has(id)));
  }

  evalDuration(evaluation: EvaluationSummary): string {
    return formatDuration(evaluationDuration(evaluation, isRunningEvaluationStatus(evaluation.status) ? this.tick() : 0));
  }

  pkgDuration(evaluation: EvaluationSummary, ep: EntryPointSummary): string {
    const running = ep.build_status === 'Building' && isRunningEvaluationStatus(evaluation.status);
    return buildDurationTextForEvaluation({ status: ep.build_status, ...ep }, evaluation, running ? this.tick() : 0);
  }

  readonly commitLabel = commitLabel;
  readonly commitWebUrl = commitWebUrl;
  readonly repositoryWebUrl = repositoryWebUrl;
  readonly splitRepository = splitRepository;

  getCommitMessageAndShortHashTooltip(e: EvaluationSummary): string {
    return e.commit_message ? `${e.commit_message} · ${commitLabel(e.commit)}` : commitLabel(e.commit);
  }

  evalTitle(e: EvaluationSummary): string {
    return evaluationTitle(e);
  }

  readonly triggerLabel = getEvaluationTriggerLabel;

  getFailedAttributeTooltipText(failure: FailedAttributeSummary): string {
    return `${failure.eval} - ${removeNixErrorInternalsFromEvaluationMessage(removeAnsiEscapeSequences(failure.message))}`;
  }

  searchTerm = signal('');
  private searchOpened = signal(false);
  private listSpansPages = signal(false);
  searchShown = computed(() => this.searchOpened() || this.listSpansPages() || this.searchTerm() !== '');
  private searchField = viewChild<ElementRef<HTMLInputElement>>('packageSearch');
  private typedTerms = new Subject<string>();
  private appliedTerms = this.typedTerms
    .pipe(debounceTime(TaskDetailComponent.SEARCH_DEBOUNCE_MS), takeUntilDestroyed())
    .subscribe(term => this.applySearch(term));
  matchingFailedAttributes = computed(() => {
    const term = this.searchTerm().toLowerCase();
    return this.failedAttributes().filter(f => f.eval.toLowerCase().includes(term));
  });
  entryPointGroups = computed(() => groupEntryPoints(this.entryPoints(), this.matchingFailedAttributes()));

  protected readonly evaluationPhase = evaluationPhase;
  protected readonly inputFetchPhase = inputFetchPhase;

  protected packagePhase(evaluation: EvaluationSummary, entry: EntryPointSummary): StatusPhase {
    if (evaluation.only_evaluate) return 'success';
    if (buildStatusToShowInEvaluation({ status: entry.build_status, ...entry }, evaluation) !== entry.build_status) return 'aborted';
    return phaseInFinishedEvaluation(entryPointPhaseWithExecJob(entry, this.execJobStatusByEntryPointAttributePath().get(entry.eval)), evaluation.status);
  }

  pkgMenuModel = signal<MenuItem[]>([]);

  retryEvaluationItem = computed<MenuItem | null>(() => {
    const selected = this.selected();
    if (!selected || !this.canRetryEvaluation(selected)) return null;
    return {
      label: 'Retry', icon: 'replay',
      disabled: this.starting(),
      disabledReason: this.evaluationInProgress() ? 'An evaluation of this task is in progress.' : null,
      command: () => this.retryEvaluation(selected),
    };
  });

  private evaluationPageItems = computed<MenuItem[]>(() => {
    const selected = this.selected();
    const disabledReason = selected ? null : NO_EVALUATION_SELECTED;
    return [
      { label: 'Logs', icon: 'article', disabledReason,
        routerLink: selected ? links.evaluationLog(this.projectName, selected.id) : undefined },
      { label: 'Summary', icon: 'summarize', disabledReason,
        routerLink: selected ? links.evaluationSummary(this.projectName, selected.id) : undefined },
    ];
  });

  private evaluationLinkItems = computed<MenuItem[]>(() => {
    const selected = this.selected();
    const job = links.boardJobOrCluster(selected?.dispatched_job, selected?.cluster_job)
      ?? (selected && isEvaluationNotStartedStatus(selected.status) ? links.boardPendingJob({ evaluation_id: selected.id }) : undefined);
    return [
      { label: 'Show job', icon: 'work', disabledReason: showJobMenuItemDisabledReason(job, this.authService.isAuthenticated(), 'evaluation'), routerLink: job },
    ];
  });

  panelMenuModel = computed<MenuItem[]>(() => [...this.evaluationLinkItems(), ...this.evaluationActionItems()]);

  evalMenuModel = computed<MenuItem[]>(() => {
    const retry = this.retryEvaluationItem();
    return [
      ...this.evaluationPageItems(),
      ...this.evaluationLinkItems(),
      ...(retry ? [retry] : []),
      ...this.evaluationActionItems(),
    ];
  });

  // Report generation is authenticated server-side, so an anonymous visitor
  // browsing a public task is not offered an action that can only 403.
  private evaluationActionItems = computed<MenuItem[]>(() => {
    const selected = this.selected();
    return [
      ...(selected && this.canPrioritizeEvaluation(selected)
        ? [{ label: 'Prioritize', icon: 'keyboard_double_arrow_up',
             command: () => this.runAction(this.evaluationsService.prioritizeEvaluation(selected.id),
               'Evaluation prioritized', 'Failed to prioritize evaluation') }]
        : []),
      ...(this.canTrigger()
        ? [{ label: 'Reevaluate all inputs', icon: 'account_tree',
             disabled: this.starting(),
             disabledReason: this.startDisabledReason(),
             command: () => this.startEvaluation('full') }]
        : []),
      ...(this.authService.isAuthenticated()
        ? [{ label: 'Diagnostic report', icon: 'bug_report', disabledReason: selected ? null : NO_EVALUATION_SELECTED,
             command: () => this.openReportDialog() }]
        : []),
    ];
  });

  protected canTrigger(): boolean {
    return this.authService.isAuthenticated() && this.triggerAccess().canEdit;
  }

  private isSuperuser(): boolean {
    return this.authService.user()?.superuser === true;
  }

  private canPrioritizeEvaluation(evaluation: EvaluationSummary): boolean {
    return this.isSuperuser() && !evaluation.prioritized && this.isRunning(evaluation.status);
  }

  private canPrioritizeEntryPoint(ep: EntryPointSummary): boolean {
    return this.isSuperuser() && canPrioritizeBuild({ status: ep.build_status, prioritized: ep.prioritized });
  }

  private canAbortEntryPoint(ep: EntryPointSummary, evaluation: EvaluationSummary): boolean {
    return this.canTrigger() && canAbortBuild(evaluation.status, ep.build_status);
  }

  private canRetryEntryPoint(ep: EntryPointSummary, evaluation: EvaluationSummary): boolean {
    const replaced = this.latestEvaluation()?.id !== evaluation.id;
    return this.canTrigger() && canRetryBuild({ status: evaluation.status, replaced }, ep.build_status);
  }

  private canRetryEvaluation(evaluation: EvaluationSummary): boolean {
    return this.canTrigger()
      && !this.isRunning(evaluation.status)
      && evaluation.builds.failed + evaluation.builds.aborted + evaluation.builds.dependency_failed > 0;
  }

  private abortBuild(ep: EntryPointSummary): void {
    const target = buildAbortTargetLine(ep.eval, this.task()?.display_name || this.taskName);
    this.confirm.confirm(abortBuildConfirm(target, () =>this.evaluationsService.abortBuild(ep.build_id))).subscribe(() => {
      this.notify.success('Build aborted');
      this.loadTaskData();
    });
  }

  private runAction(request: Observable<string>, success: string, failure: string): void {
    request.subscribe({
      next: () => {
        this.notify.success(success);
        this.loadTaskData();
      },
      error: (error: unknown) => this.notify.failure(failure, error),
    });
  }

  reportDialogOpen = signal(false);
  diagnosticReportEvaluation = signal<EvaluationSummary | null>(null);
  reportBusy = signal(false);
  reportError = signal<string | null>(null);
  reportOptions = signal<ReportOptions>({
    include_identities: false,
    include_packages: true,
    include_logs: false,
    include_instance: false,
    include_exec: false,
  });

  setReportOption(key: keyof ReportOptions, value: boolean): void {
    this.reportOptions.update(o => ({ ...o, [key]: value }));
  }

  openReportDialog(): void {
    this.diagnosticReportEvaluation.set(this.selected());
    this.reportError.set(null);
    this.reportDialogOpen.set(true);
  }

  generateReport(): void {
    const evaluation = this.diagnosticReportEvaluation();
    if (!evaluation || this.reportBusy()) return;

    this.reportBusy.set(true);
    this.reportError.set(null);
    this.tasksService.downloadReport(evaluation.id, this.reportOptions()).subscribe({
      next: response => {
        this.reportBusy.set(false);
        this.reportDialogOpen.set(false);
        saveResponse(response, `gradient-report-${evaluation.id.split('-')[0]}.db`);
      },
      error: (error: unknown) => {
        this.reportBusy.set(false);
        this.reportError.set(apiErrorMessage(error, 'Failed to generate the diagnostic report.'));
      },
    });
  }

  private downloadSbom(ep: EntryPointSummary): void {
    this.evaluationsService.downloadBuildSbom(ep.build_id).subscribe({
      next: response => saveResponse(response, `${ep.derivation_path.replace(/\.drv$/, '')}-sbom.cdx.json`),
      error: (error: unknown) => this.notify.failure('Failed to download SBOM', error),
    });
  }

  private buildPkgMenu(ep: EntryPointSummary, evalId: string): MenuItem[] {
    const evaluation = this.evaluations().find(e => e.id === evalId);
    const from = { task: this.taskName, evaluation: evalId };
    return [
      {
        label: 'Artefacts', icon: 'download', disabledReason: getArtefactsMenuItemDisabledReason(ep.build_status, ep.has_artefacts),
        routerLink: links.artefacts(this.projectName, ep.build_id, from),
      },
      {
        label: 'Dependency graph', icon: 'account_tree',
        routerLink: links.dependencyGraph(this.projectName, ep.build_id, from),
      },
      {
        label: 'View closure', icon: 'hub', disabledReason: getClosureMenuItemDisabledReason(ep.build_status),
        routerLink: links.closure(this.projectName, 'build', ep.build_id, from),
      },
      { label: 'SBOM', icon: 'inventory_2', disabledReason: getSbomDownloadMenuItemDisabledReason(ep.build_status), command: () => this.downloadSbom(ep) },
      {
        label: 'Entry-point metrics', icon: 'show_chart',
        routerLink: links.entryPointMetrics(this.projectName, this.taskName, ep.eval),
      },
      ...(this.canPrioritizeEntryPoint(ep)
        ? [{ label: 'Prioritize', icon: 'keyboard_double_arrow_up',
             command: () => this.runAction(this.evaluationsService.prioritizeBuild(ep.build_id),
               'Build prioritized', 'Failed to prioritize build') }]
        : []),
      ...(evaluation && this.canAbortEntryPoint(ep, evaluation)
        ? [{ label: 'Abort', icon: 'stop_circle', command: () => this.abortBuild(ep) }]
        : []),
      ...(evaluation && this.canRetryEntryPoint(ep, evaluation)
        ? [{ label: 'Retry', icon: 'replay',
             command: () => this.runAction(this.evaluationsService.retryBuild(ep.build_id),
               'Build retried', 'Failed to retry build') }]
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

  protected readonly buildTally = buildTally;
  protected readonly getBuildCountTextPieces = getBuildCountTextPieces;

  protected buildCountTitle(tally: BuildTally): string {
    const line = getBuildCountTextPieces(tally).map((piece) => piece.text).join(' · ');
    return tally.overall > tally.total ? `${line} · ${tally.overall} build count (substituted builds included)` : line;
  }
  protected readonly plural = plural;
  protected readonly abortedWithoutBuildsTitle = abortedWithoutBuildsTitle;

  /// Dep-closure counts plus the entry point's own build, so a package with
  /// few or no deps still shows its own progress in the bar.
  barCounts(ep: EntryPointSummary): BuildStatusCounts {
    const known = this.barCountsByEntryPoint.get(ep);
    if (known) return known;
    const c = { ...ep.deps };
    switch (ep.build_status) {
      case 'Completed': c.completed++; break;
      case 'Substituted': c.substituted++; break;
      case 'FailedPermanent': case 'FailedTimeout': c.failed++; break;
      case 'Aborted': c.aborted++; break;
      case 'DependencyFailed': c.dependency_failed++; break;
      case 'Building': c.building++; break;
      // Settled work with no result: it is not pending, so it draws nothing.
      case 'Skipped': break;
      default: c.queued++;
    }
    this.barCountsByEntryPoint.set(ep, c);
    return c;
  }
}
