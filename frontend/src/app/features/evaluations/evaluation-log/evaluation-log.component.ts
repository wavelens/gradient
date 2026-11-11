/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  Component,
  OnInit,
  OnDestroy,
  inject,
  signal,
  computed,
  ViewChild,
  DestroyRef,
  HostListener,
  ChangeDetectionStrategy
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { LogViewerComponent, ServerLog, ansiToHtml, isFindShortcut, splitLogLines } from '@shared/log';
import { matchesBuildSearch } from './build-search';
import { BuildLogStreamDecoder, buildBuildLogApiPath, buildLog, fetchBuildLogResponseWithCookiesOrThrow } from './build-log';
import { lastRateLimitNote, rateLimitMarker } from './log-rate-limit';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule, UrlTree } from '@angular/router';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';
import { EMPTY, Observable, Subscription, firstValueFrom, forkJoin, merge, of } from 'rxjs';
import { auditTime, catchError, filter, map, share, switchMap, tap } from 'rxjs/operators';
import { EvaluationsService } from '@core/services/evaluations.service';
import { LiveEvent, LiveService, createResyncOnLiveSocketReconnectCallback } from '@core/services/live.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TasksService } from '@core/services/tasks.service';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { ExecJobListComponent } from '@features/exec-jobs/exec-job-list/exec-job-list.component';
import { ConfirmService } from '@shared/confirm';
import { AccessService, WritableDirective } from '@shared/access';
import { BuildOrigin, CopyMenuService, links } from '@shared/routing';
import { CurrentTimeSignal, formatDuration, packageNameFromDerivationPath, plural, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { AccessState, accessFromEntity } from '@core/models/access.model';
import {
  BuildAttemptResponse,
  BuildProgress,
  EvaluationResponse,
  TaskDetailsResponse,
  EvaluationProgress,
  BuildItem,
  BuildStatus,
  EvaluationMessageResponse,
  LogChunkIndex,
  EvaluationStatus,
  BuildWithOutputs,
  DependencyGraphNode,
  ExecJobSummary,
  PaginatedBuilds,
  Permission,
} from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { EvaluationWithOptionalTask } from '@core/resolvers/evaluation-access.resolver';
import {
  BadgeComponent,
  ButtonComponent,
  LoadStateComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  LoadMoreComponent,
  MenuComponent,
  MenuItem,
  MessageBannerComponent,
  MessageService,
  DrawerDirective,
  SelectComponent,
  SkeletonComponent,
} from '@gradient/ui/ui';
import { BuildProgressComponent, EvalStatusBadgeComponent, InputFetchListComponent, LiveStatusComponent, ThunkProgressComponent, WaitingNoticeComponent } from '@shared/ui';
import { abortBuildConfirm, buildStatusNameFromStatusCode, abortedWithoutBuildsTitle, abortEvaluationConfirm, buildAbortTargetLine, evaluationAbortTarget, buildDurationTextForEvaluation, buildPhase, buildOutcomeLabel, buildPhaseFinished, buildStatusToShowInEvaluation, canAbortBuild, canPrioritizeBuild, canRetryBuild, commitLabel, formatAttemptDropdownLabel, getAbortedEvaluationExplanationWithReplacementId, getArtefactsMenuItemDisabledReason, getClosureMenuItemDisabledReason, getDownloadLogMenuItemDisabledReason, getSbomDownloadMenuItemDisabledReason, showJobMenuItemDisabledReason, evaluationDuration, getEvaluationTriggerLabel, isPendingBuildStatus, isRunningEvaluationStatus, phaseProgress, thunkProgress, waitingAccessFrom, getEvaluationMessageSourceLabel, removeNixErrorInternalsFromEvaluationMessage, PHASE_COLOR, type StatusPhase } from '@shared/evaluation';
import { saveResponse } from '@shared/download';

const packageNameVisibleTrailingCharacterCount = 10;
const emptyBuildPageForEvaluationOnly: PaginatedBuilds = { builds: [], total: 0, active_count: 0 };

@Component({
  selector: 'app-evaluation-log',
  standalone: true,
  imports: [CommonModule, FormsModule, SelectComponent, ServerTimestampToLocalDateTimePipe, RouterModule, LoadingSpinnerComponent, LoadMoreComponent,LoadStateComponent, ButtonComponent, IconComponent, BadgeComponent, SkeletonComponent, BuildProgressComponent, EvalStatusBadgeComponent, ExecJobListComponent, InputDirective, InputFetchListComponent, LiveStatusComponent, LogViewerComponent, MenuComponent, MessageBannerComponent, ThunkProgressComponent, WaitingNoticeComponent, WritableDirective, DrawerDirective],
  templateUrl: './evaluation-log.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: [
    '../../../shared/log/log-page.scss',
    '../../../shared/log/log-sidebar.scss',
    '../../../shared/evaluation/phone-breadcrumb.scss',
    './evaluation-log.component.scss',
    './evaluation-log.sidebar.scss',
    './evaluation-log.messages.scss',
    './evaluation-log.log.scss',
  ],
})
export class EvaluationLogComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private live = inject(LiveService);
  private crumbs = inject(BreadcrumbsService);
  private tasksService = inject(TasksService);
  private accessService = inject(AccessService);
  private messageService = inject(MessageService);
  private notify = inject(NotifyService);
  private copyMenuService = inject(CopyMenuService);
  private confirm = inject(ConfirmService);
  private execJobs = inject(ExecJobsService);
  protected authService = inject(AuthService);
  private sanitizer = inject(DomSanitizer);
  private destroyRef = inject(DestroyRef);

  @ViewChild(LogViewerComponent) logViewer?: LogViewerComponent;

  loading = signal(true);
  loadError = signal<unknown>(null);
  messagesLoading = signal(false);
  messagesError = signal<unknown>(null);
  evaluation = signal<EvaluationResponse | null>(null);
  private liveProgress = signal<EvaluationProgress | null>(null);
  progress = computed(() => {
    const ev = this.evaluation();
    return ev ? phaseProgress(ev.status, this.liveProgress(), ev.progress) : null;
  });
  thunkRow = computed(() => thunkProgress(this.progress(), this.evaluation()?.expected_thunks));
  fetchRows = computed(() => {
    const p = this.progress();
    return p?.kind === 'fetching' ? p.inputs : [];
  });
  builds = signal<BuildItem[]>([]);
  messages = signal<(EvaluationMessageResponse & { renderedHtml: SafeHtml; sourceLabel: string | null })[]>([]);
  selectedBuildId = signal<string | null>(null);
  selectedSection = signal<'messages' | null>(null);
  buildsOpen = signal(false);
  logLineCount = signal(0);
  logLoading = signal(true);
  logRequestError = signal<unknown>(null);
  buildListError = signal<unknown>(null);
  hasLoadedFirstBuildPage = signal(false);
  aborting = signal(false);
  liveConnectionOpen = signal<boolean | null>(null);
  // The log page is reachable without the task route's access resolver, so the
  // owning task is asked directly: a viewer must not be offered an abort.
  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));
  private taskPermissions = signal<readonly Permission[]>([]);
  waitingAccess = computed(() =>
    waitingAccessFrom(this.taskPermissions(), this.access().canTrigger, this.authService.user()?.superuser === true),
  );
  private clock = inject(CurrentTimeSignal);
  duration = computed(() => {
    const evaluation = this.evaluation();
    return evaluation ? formatDuration(evaluationDuration(evaluation, this.clock.currentTimeMilliseconds())) : '';
  });
  pendingJobLink = computed(() => (this.authService.isAuthenticated() ? links.boardPendingJob({ evaluation_id: this.evaluationId }) : null));
  activeBuildsCount = signal(0);
  execJobsWaitingWithReason = signal<ExecJobSummary[]>([]);
  execJobStartingId = signal<string | null>(null);

  projectName = '';
  evaluationLogBuildOrigin(): BuildOrigin {
    return { task: this.evaluation()?.task_name, evaluation: this.evaluationId };
  }

  breadcrumb = computed(() => {
    const task = this.evaluation()?.task_name;
    return task
      ? this.crumbs.buildEvaluationPageBreadcrumbs(this.projectName, this.evaluationId, task)
      : this.crumbs.project(this.projectName);
  });
  projectAndTaskCrumbs = computed(() => this.breadcrumb().slice(1).filter((crumb) => crumb.link));
  evaluationId = '';
  private initialBuildId: string | null = null;
  missingBuildId = signal<string | null>(null);
  // #489: when set, the build list is scoped to this build's package closure.
  scopeBuildId = signal<string | null>(null);
  private initialShowEval = false;
  private fetchingInitialBuild = false;

  selectedBuild = computed(() =>
    this.builds().find(b => b.id === this.selectedBuildId()) ?? null
  );

  selectedBuildAttempts = signal<BuildAttemptResponse[]>([]);
  selectedAttemptNumberOrNull = signal<number | null>(null);
  private initialAttemptNumber: number | null = null;
  attemptDropdownOptions = computed(() => this.selectedBuildAttempts().map((attempt) => ({
    label: formatAttemptDropdownLabel(attempt.number, attempt.outcome, attempt.finished_at ?? attempt.started_at),
    value: attempt.number,
  })));
  attemptNumberForLogOnScreenOrNull = computed(() =>
    this.selectedAttemptNumberOrNull() ?? this.selectedBuildAttempts().reduce<number | null>((latest, a) => Math.max(latest ?? 0, a.number), null),
  );
  buildEvaluationLogLineShareLink = (line: number): UrlTree =>
    links.buildLine(this.projectName, this.evaluationId, this.selectedBuildId() ?? '', this.attemptNumberForLogOnScreenOrNull(), line);

  abortedEvaluationExplanationWithReplacementIdForHeaderEvaluation = computed(() => {
    const evaluation = this.evaluation();
    return evaluation ? getAbortedEvaluationExplanationWithReplacementId(evaluation) : null;
  });

  limitingBuildDisplayName = computed(() => {
    const id = this.scopeBuildId();
    const build = id ? this.builds().find(b => b.id === id) : undefined;
    return build ? this.buildDisplayName(build.name) : null;
  });

  failedDependencies = signal<DependencyGraphNode[]>([]);

  buildOf(node: DependencyGraphNode): BuildItem | null {
    return this.builds().find((b) => b.id === node.build) ?? null;
  }

  // The build the sidebar's menu is acting on, opened by right-click or the
  // row's three dots.
  contextBuild = signal<BuildItem | null>(null);

  buildMenuModel = computed<MenuItem[]>(() => {
    const build = this.contextBuild();
    if (!build) return [];

    const from = this.evaluationLogBuildOrigin();
    const job = build.dispatched_job;
    return [
      {
        label: 'Dependency graph',
        icon: 'account_tree',
        routerLink: links.dependencyGraph(this.projectName, build.id, from),
      },
      {
        label: 'Closure',
        icon: 'hub',
        disabledReason: getClosureMenuItemDisabledReason(build.status),
        routerLink: links.closure(this.projectName, 'build', build.id, from),
      },
      {
        label: 'Show job',
        icon: 'work',
        disabledReason: showJobMenuItemDisabledReason(job, this.authService.isAuthenticated(), 'build'),
        routerLink: job ? links.boardJob(job) : undefined,
      },
      {
        label: 'Artefacts',
        icon: 'inventory_2',
        disabledReason: getArtefactsMenuItemDisabledReason(build.status, build.has_artefacts),
        routerLink: links.artefacts(this.projectName, build.id, from),
      },
      { separator: true },
      {
        label: 'Download Log',
        icon: 'download',
        disabledReason: getDownloadLogMenuItemDisabledReason(build.status),
        command: () => void this.downloadLog(build),
      },
      {
        label: 'SBOM',
        icon: 'description',
        disabledReason: getSbomDownloadMenuItemDisabledReason(build.status),
        command: () => this.downloadSbom(build),
      },
      this.copyMenuService.createCopyMenuItemWithCliCommandApiUrlAndLink({
        gradientCliCommand: `gradient builds log ${build.id}`,
        relativeApiEndpointPath: `builds/${build.id}/log?format=plain`,
        absoluteLinkUrlTree: links.evaluationLog(this.projectName, this.evaluationId, { build: build.id }),
      }),
      ...(this.canPrioritize(build)
        ? [{ label: 'Prioritize', icon: 'keyboard_double_arrow_up', command: () => this.prioritizeBuild(build) }]
        : []),
      ...(this.canAbort(build)
        ? [{ label: 'Abort', icon: 'stop_circle', command: () => this.abortBuild(build) }]
        : []),
      ...(this.canRetry(build)
        ? [{ label: 'Retry', icon: 'replay', command: () => this.retryBuild(build) }]
        : []),
    ];
  });

  buildProgress = signal<BuildProgress | null>(null);
  shownProgress = computed(() => {
    const p = this.buildProgress();
    return p && !buildPhaseFinished(p) ? p : null;
  });
  downloadProgress = computed(() => this.shownProgress()?.phase === 'download' ? this.shownProgress() : null);
  transferProgress = computed(() => this.shownProgress()?.phase === 'download' ? null : this.shownProgress());

  errorMessages = computed(() => this.messages().filter(m => m.level === 'Error'));
  warningMessages = computed(() => this.messages().filter(m => m.level === 'Warning'));

  private liveSub?: Subscription;
  private progressSub?: Subscription;
  private logStreamAbortController?: AbortController;
  buildIdWithEndedLogStream = signal<string | null>(null);
  private buildLogStreamReconnectAttemptsSinceLastOutput = 0;
  private logStreamReconnectTimer?: ReturnType<typeof setTimeout>;
  private autoFollowBuilding = false;
  private userPickedBuild = false;
  private isInitialBuildsLoad = true;
  private readonly latestLiveBuildStatusByBuildId = new Map<string, BuildStatus>();
  private pendingLogLines: string[] = [];
  private logDrainTimer?: ReturnType<typeof setInterval>;
  private readonly PAGE_SIZE = 1000;
  protected readonly totalBuilds = signal(0);
  protected readonly loadingMore = signal(false);
  protected readonly plural = plural;

  logLines = signal<readonly string[]>([]);
  logPaused = signal(false);
  protected readonly rateLimitMarker = rateLimitMarker;
  serverLog = signal<ServerLog | null>(null);
  sidebarSearchOpen = signal(false);
  sidebarSearchQuery = signal('');
  private sidebarFocused = false;
  private pointerOver: 'builds' | 'log' | null = null;
  private readonly DRAIN_MS = 250;

  ngOnInit(): void {
    document.documentElement.style.overflow = 'hidden';
    document.body.style.overflow = 'hidden';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.evaluationId = this.route.snapshot.paramMap.get('evaluationId') || '';
    const query = this.route.snapshot.queryParamMap;
    this.scopeBuildId.set(query.get('filter'));
    this.initialShowEval = query.get('eval') !== null;
    this.initialBuildId = this.initialShowEval ? null : query.get('build') ?? query.get('filter');
    const attempt = Number(query.get('attempt'));
    this.initialAttemptNumber = Number.isInteger(attempt) && attempt > 0 ? attempt : null;
    const resolved = this.route.snapshot.data?.['evaluationAccess'] as EvaluationWithOptionalTask | undefined;
    if (!resolved) return this.loadEvaluation();
    this.setEvaluationAndStartLiveUpdates(resolved.evaluation);
    if (resolved.task) this.setEvaluationAccessFromTask(resolved.task);
  }

  ngOnDestroy(): void {
    document.documentElement.style.overflow = '';
    document.body.style.overflow = '';
    this.stopLiveUpdates();
    this.stopActiveStream();
  }

  loadEvaluation(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.evalService.getEvaluation(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (evaluation) => {
        this.setEvaluationAndStartLiveUpdates(evaluation);
        this.loadAccess(evaluation.task_name);
      },
      error: (error: unknown) => {
        if (!(error instanceof ApiError && error.status === 404)) this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private setEvaluationAndStartLiveUpdates(evaluation: EvaluationResponse): void {
    this.rememberTaskName(evaluation);
    this.evaluation.set(evaluation);
    this.loading.set(false);
    if (this.initialBuildId) {
      this.resolveInitialBuild(() => this.loadBuilds());
    } else {
      this.loadBuilds();
    }
    this.loadMessages();
    this.startLiveUpdates(evaluation.status);
  }

  private refreshEvaluation(): void {
    this.evalService.getEvaluation(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe(evaluation => {
      this.evaluation.set(evaluation);
      this.loadBuilds(Math.max(this.PAGE_SIZE, this.builds().length));
      if (this.isRunningStatus(evaluation.status) && !this.liveSub) {
        this.startLiveUpdates(evaluation.status);
      }
    });
  }

  private rememberTaskName(evaluation: EvaluationResponse): void {
    if (!evaluation.task_name || !evaluation.task_display_name) return;
    this.crumbs.rememberTask(this.projectName, evaluation.task_name, evaluation.task_display_name);
  }

  private loadAccess(taskName?: string | null): void {
    if (!taskName) return;
    this.tasksService
      .getTask(this.projectName, taskName)
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (task) => this.setEvaluationAccessFromTask(task),
        error: () => {},
      });
  }

  private setEvaluationAccessFromTask(task: TaskDetailsResponse): void {
    this.access.set(accessFromEntity(task));
    this.taskPermissions.set(task.permissions);
  }

  loadMessages(background = false): void {
    if (!background) {
      this.messagesLoading.set(true);
      this.messagesError.set(null);
    }
    this.evalService.getEvaluationMessages(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (msgs) => {
        this.messages.set(
          msgs.map(m => ({
            ...m,
            renderedHtml: this.sanitizer.bypassSecurityTrustHtml(ansiToHtml(removeNixErrorInternalsFromEvaluationMessage(m.message))),
            sourceLabel: getEvaluationMessageSourceLabel(m.source),
          }))
        );
        this.messagesError.set(null);
        this.messagesLoading.set(false);
      },
      error: (error: unknown) => {
        if (!background) this.messagesError.set(error);
        this.messagesLoading.set(false);
      },
    });
  }

  private readonly buildStatusOrder: Record<string, number> = {
    building: 0,
    failed: 1,
    dependencyfailed: 2,
    aborted: 3,
    queued: 4,
    completed: 5,
    substituted: 5,
    skipped: 6,
  };

  /// The sidebar sections follow display order. Failed and skipped builds come right after Building. Sections get their label from buildOutcomeLabel. Rows for a status other than the section label show their own status label, for example a timed out build under Failed. Statuses this list doesn't recognize map to `Other`. All builds appear in the list.
  private readonly buildGroups: { key: string; label: string; members: string[]; phase: StatusPhase }[] = [
    { key: 'building', label: buildOutcomeLabel('Building'), members: ['building'], phase: 'running' },
    { key: 'failed', label: buildOutcomeLabel('FailedPermanent'), members: ['failed'], phase: 'failure' },
    { key: 'dependencyfailed', label: buildOutcomeLabel('DependencyFailed'), members: ['dependencyfailed'], phase: 'aborted' },
    { key: 'aborted', label: buildOutcomeLabel('Aborted'), members: ['aborted'], phase: 'aborted' },
    { key: 'queued', label: buildOutcomeLabel('Queued'), members: ['queued'], phase: 'queued' },
    { key: 'completed', label: buildOutcomeLabel('Completed'), members: ['completed'], phase: 'success' },
    { key: 'substituted', label: buildOutcomeLabel('Substituted'), members: ['substituted'], phase: 'success' },
    { key: 'skipped', label: buildOutcomeLabel('Skipped'), members: ['skipped'], phase: 'aborted' },
    { key: 'other', label: 'Other', members: [], phase: 'aborted' },
  ];

  collapsedGroups = signal<ReadonlySet<string>>(new Set());

  /// `builds` bucketed into the status sections above (only non-empty
  /// ones), in the flat status-sorted order keyboard navigation walks.
  groupedBuilds = computed(() => {
    const indexByGroup = new Map<string, number>();
    this.buildGroups.forEach((g, i) => g.members.forEach((m) => indexByGroup.set(m, i)));
    const buckets: BuildItem[][] = this.buildGroups.map(() => []);
    for (const build of this.builds()) {
      if (!matchesBuildSearch(build.name, this.sidebarSearchQuery())) continue;
      buckets[indexByGroup.get(this.statusClass(this.getBuildRowDisplayStatus(build))) ?? buckets.length - 1].push(build);
    }
    const collapsed = this.collapsedGroups();
    return this.buildGroups
      .map((g, i) => ({ key: g.key, label: g.label, color: PHASE_COLOR[g.phase], builds: buckets[i], collapsed: collapsed.has(g.key) }))
      .filter((g) => g.builds.length > 0);
  });

  private navigableBuilds = computed(() =>
    this.groupedBuilds().flatMap((g) => (g.collapsed ? [] : g.builds)),
  );

  toggleGroup(key: string): void {
    this.collapsedGroups.update((cur) => {
      const next = new Set(cur);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }

  getBuildRowDisplayStatus(build: Pick<BuildItem, 'status' | 'build_started_at' | 'build_time_ms'>): string {
    const evaluation = this.evaluation();
    return evaluation ? buildStatusToShowInEvaluation(build, evaluation) : build.status;
  }

  buildStatusColor(status: string | null | undefined): string {
    return PHASE_COLOR[(status && buildPhase(status as BuildStatus)) || 'aborted'];
  }

  statusClass(status: string | null | undefined): string {
    switch (status) {
      case 'Completed': return 'completed';
      case 'Substituted': return 'substituted';
      case 'Building': return 'building';
      case 'Queued':
      case 'Created': return 'queued';
      case 'FailedPermanent':
      case 'FailedTransient':
      case 'FailedTimeout': return 'failed';
      case 'DependencyFailed': return 'dependencyfailed';
      case 'Aborted': return 'aborted';
      case 'Skipped': return 'skipped';
      default: return (status ?? '').toLowerCase();
    }
  }

  private sortBuilds(builds: BuildItem[]): BuildItem[] {
    // Key by derivation path, not id: follower builds (via != null) are surfaced
    // under the leader's id by the API, but a deep-linked follower keeps its own
    // id, so the same logical build can arrive under two ids. The derivation path
    // is the stable logical identity. Prefer the entry already selected so a
    // deep-link survives, otherwise the first seen wins.
    const byKey = new Map<string, BuildItem>();
    const selectedId = this.selectedBuildId();
    for (const b of builds) {
      const key = b.name;
      const existing = byKey.get(key);
      if (!existing || b.id === selectedId) byKey.set(key, b);
    }

    // The API's order: status, then dependency layer, then name within a layer
    // (#614), so each section reads like the dependency graph page. Re-applied
    // rather than taken as given because builds also arrive from live updates
    // and deep links, out of the paged order.
    return [...byKey.values()].sort((a, b) => {
      const oa = this.buildStatusOrder[this.statusClass(this.getBuildRowDisplayStatus(a))] ?? 99;
      const ob = this.buildStatusOrder[this.statusClass(this.getBuildRowDisplayStatus(b))] ?? 99;
      if (oa !== ob) return oa - ob;
      if (a.depth !== b.depth) return a.depth - b.depth;
      return this.buildDisplayName(a.name).localeCompare(this.buildDisplayName(b.name), undefined, { numeric: true });
    });
  }

  loadBuilds(limit = this.PAGE_SIZE): void {
    this.firstBuildPage(limit).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (result) => this.mergeBuildPageAndSelectOrFollowBuild(result),
      error: (error: unknown) => this.buildListError.set(error),
    });
  }

  private firstBuildPage(limit = this.PAGE_SIZE): Observable<PaginatedBuilds> {
    if (this.evaluation()?.only_evaluate) return of(emptyBuildPageForEvaluationOnly);
    return this.evalService.getBuilds(this.evaluationId, limit, 0, this.scopeBuildId() ?? undefined);
  }

  private mergeBuildPageAndSelectOrFollowBuild(result: PaginatedBuilds): void {
    this.buildListError.set(null);
    this.hasLoadedFirstBuildPage.set(true);
    this.totalBuilds.set(result.total);
    this.activeBuildsCount.set(result.active_count);
    if (!this.isRunning() && result.active_count === 0) this.stopLiveUpdates();
    const prevSelected = this.selectedBuild();

    const byId = new Map(this.builds().map((b) => [b.id, b]));
    for (const [id, status] of this.latestLiveBuildStatusByBuildId) {
      const known = byId.get(id);
      if (known) byId.set(id, { ...known, status });
    }
    this.latestLiveBuildStatusByBuildId.clear();
    for (const fresh of result.builds) byId.set(fresh.id, fresh);
    this.builds.set(this.sortBuilds([...byId.values()]));

    const newSelected = this.selectedBuild();

    if (this.isInitialBuildsLoad) {
      this.isInitialBuildsLoad = false;
      const evalStatus = this.evaluation()?.status;
      const failedWithNoBuilds = result.total === 0 && evalStatus === 'Failed';
      if (this.initialShowEval || failedWithNoBuilds) {
        // Open the evaluation messages view directly (explicit request via ?eval,
        // or implicit fallback when there is nothing to build).
        this.initialShowEval = false;
        this.selectEvaluationSection();
      } else if (!this.initialBuildId && !this.selectedBuildId()) {
        const firstBuilding = this.builds().find(b => b.status === 'Building');
        const firstFailed = this.builds().find(b => this.statusClass(b.status) === 'failed');
        const defaultEntryPointBuild = evalStatus === 'Completed' ? this.builds().find((b) => b.depth === 0) : undefined;
        const first = firstBuilding ?? firstFailed ?? defaultEntryPointBuild;
        if (first) this.selectBuild(first);
        else if (evalStatus === 'Aborted' && result.total > 0) this.selectEvaluationSection();
        if (!firstBuilding) this.autoFollowBuilding = true;
      }
    }

    // Building -> Completed: flush logs.
    // Only auto-switch to the next building build when in auto-follow mode
    // (the user hasn't manually picked a build). When the user explicitly
    // selected or navigated to this build, stay on it so they can read the log.
    if (prevSelected?.status === 'Building' && newSelected?.status === 'Completed') {
      this.flushPendingLogs();
      if (!this.userPickedBuild) {
        const next = this.builds().find(b => b.status === 'Building');
        if (next) {
          this.selectBuild(next);
        } else {
          this.autoFollowBuilding = true;
        }
      }
    }

    // Auto-select the first building build while waiting for one to start
    if (this.autoFollowBuilding) {
      const next = this.builds().find(b => b.status === 'Building');
      if (next) {
        this.autoFollowBuilding = false;
        this.selectBuild(next);
      }
    }

    this.resolveInitialBuild();

    // Auto-fetch more pages until all active builds are in memory.
    // This ensures status transitions and log streaming work for every build.
    if (this.builds().length < result.active_count && !this.loadingMore()) {
      this.doLoadMore();
    }
  }

  /** Triggered by scroll proximity - pre-fetches next page before user hits bottom. */
  loadMoreBuilds(): void {
    if (this.loadingMore() || this.builds().length >= this.totalBuilds()) return;
    this.doLoadMore();
  }

  private doLoadMore(): void {
    if (this.loadingMore() || this.builds().length >= this.totalBuilds()) return;
    this.loadingMore.set(true);
    const offset = this.builds().length;

    this.evalService.getBuilds(this.evaluationId, this.PAGE_SIZE, offset, this.scopeBuildId() ?? undefined).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (result) => {
        // Don't update activeBuildsCount here. loadBuilds() is the only owner of this signal, so a response that comes back at the same time can't change the value unexpectedly.
        this.totalBuilds.set(result.total);
        if (result.builds.length > 0) {
          const existingIds = new Set(this.builds().map(b => b.id));
          const newBuilds = result.builds.filter(b => !existingIds.has(b.id));
          if (newBuilds.length > 0) {
            this.builds.update(current => this.sortBuilds([...current, ...newBuilds]));
          }
        }
        this.loadingMore.set(false);

        // A build paged in here may be the one we deep-linked to.
        this.resolveInitialBuild();

        // Continue fetching if there are still active builds not yet in memory
        if (this.builds().length < result.active_count) {
          this.doLoadMore();
        }
      },
      error: () => {
        this.loadingMore.set(false);
      },
    });
  }

  startLiveUpdates(status: EvaluationStatus): void {
    this.stopLiveUpdates();
    if (!this.isRunningStatus(status)) return;

    const frames = this.live.connect<LiveEvent>(
      `/evals/${this.evaluationId}/live`,
      createResyncOnLiveSocketReconnectCallback(() => this.refreshEvaluation(), (open) => this.liveConnectionOpen.set(open)),
    ).pipe(share());
    this.liveSub = frames
      .pipe(filter(e => e.event === 'evaluation.activity'))
      .subscribe(e => this.liveProgress.set(e.content.progress ?? null));
    this.liveSub.add(
      frames
        .pipe(
          filter(e => e.event !== 'evaluation.activity'),
          tap((e) => {
            const status = buildStatusNameFromStatusCode(e.content.status);
            if (e.content.build_id && status) this.latestLiveBuildStatusByBuildId.set(e.content.build_id, status);
          }),
          auditTime(300),
          switchMap(() => forkJoin({ evaluation: this.evalService.getEvaluation(this.evaluationId), page: this.firstBuildPage() }).pipe(
            catchError((error: unknown) => {
              this.buildListError.set(error);
              return EMPTY;
            }),
          )),
        )
        .subscribe({
          next: ({ evaluation, page }) => {
            const finishedNow = this.isRunning() && !this.isRunningStatus(evaluation.status);
            this.evaluation.set(evaluation);
            this.mergeBuildPageAndSelectOrFollowBuild(page);
            if (!finishedNow) return;
            this.loadMessages(true);
            this.stopLiveUpdates();
          },
        }),
    );
  }

  stopLiveUpdates(): void {
    this.liveSub?.unsubscribe();
    this.liveSub = undefined;
    this.liveConnectionOpen.set(null);
  }

  setSidebarFocus(v: boolean): void { this.sidebarFocused = v; }
  setPointerOver(region: 'builds' | 'log' | null): void { this.pointerOver = region; }

  // ── Build selection & log loading ──────────────────────────────────────────

  selectEvaluationSection(): void {
    this.buildsOpen.set(false);
    this.selectedSection.set('messages');
    this.selectedBuildId.set(null);
    this.stopActiveStream();
    this.resetLogState();
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { build: null, attempt: null, eval: 1 },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  removeBuildLimit(): void {
    this.scopeBuildId.set(null);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { filter: null },
      queryParamsHandling: 'merge',
      preserveFragment: true,
      replaceUrl: true,
    });
    this.isInitialBuildsLoad = true;
    this.loadBuilds();
  }

  /// Resolves a deep-linked `?build=<id>`. If the build is already loaded we
  /// select it; otherwise we fetch that single build directly and inject it into
  /// the sidebar so its log shows immediately, rather than paging through the
  /// whole evaluation. `sortBuilds` dedups it once pagination catches up.
  private resolveInitialBuild(settled?: () => void): void {
    const id = this.initialBuildId;
    if (!id) return;
    const target = this.builds().find(b => b.id === id);
    if (target) {
      this.initialBuildId = null;
      this.selectBuild(target, true);
      return;
    }
    if (this.fetchingInitialBuild) return;
    this.fetchingInitialBuild = true;
    this.evalService.getBuild(id).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (b: BuildWithOutputs) => {
        this.fetchingInitialBuild = false;
        if (this.initialBuildId !== id) return settled?.();
        if (b.evaluation !== this.evaluationId) return this.dropMissingInitialBuild(id, settled);
        const item: BuildItem = {
          id: b.id,
          name: b.derivation_path,
          status: b.status,
          has_artefacts: false,
          updated_at: b.updated_at,
          build_time_ms: null,
          build_started_at: null,
          dispatched_job: b.dispatched_job,
          prioritized: b.prioritized,
          ifd: b.ifd,
          depth: 0,
          entry_points: b.entry_points,
        };
        this.initialBuildId = null;
        this.builds.update(cur => this.sortBuilds([item, ...cur]));
        this.selectBuild(item, true);
        settled?.();
      },
      error: (error: unknown) => {
        this.fetchingInitialBuild = false;
        if (error instanceof ApiError && (error.status === 404 || error.status === 403)) return this.dropMissingInitialBuild(id, settled);
        settled?.();
      },
    });
  }

  private dropMissingInitialBuild(id: string, settled?: () => void): void {
    this.initialBuildId = null;
    this.missingBuildId.set(id);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { build: null },
      queryParamsHandling: 'merge',
      preserveFragment: true,
      replaceUrl: true,
    });
    settled?.();
  }

  selectBuild(build: BuildItem, isUserAction = false): void {
    this.buildsOpen.set(false);
    this.selectedSection.set(null);
    if (this.selectedBuildId() === build.id) return;

    this.userPickedBuild = isUserAction;
    if (isUserAction) this.missingBuildId.set(null);
    this.autoFollowBuilding = false;
    this.stopActiveStream();
    this.buildIdWithEndedLogStream.set(null);
    this.buildLogStreamReconnectAttemptsSinceLastOutput = 0;
    const keepLineLinkUntilBuildLogShown = this.logLineCount() === 0;
    this.resetLogState();
    this.selectedBuildAttempts.set([]);
    this.selectedBuildId.set(build.id);
    this.selectedAttemptNumberOrNull.set(this.initialAttemptNumber);
    this.initialAttemptNumber = null;

    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { build: build.id, attempt: this.selectedAttemptNumberOrNull(), eval: null },
      queryParamsHandling: 'merge',
      preserveFragment: keepLineLinkUntilBuildLogShown,
      replaceUrl: true,
    });

    this.logLoading.set(true);
    this.fetchInitialLogs(build.id);
    this.loadFailedDependencies(build);
  }

  private loadAttempts(buildId: string): void {
    this.evalService.getBuildAttempts(buildId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (attempts) => {
        if (this.selectedBuildId() === buildId) this.selectedBuildAttempts.set(attempts);
      },
      error: () => {},
    });
  }

  selectAttempt(attempt: number): void {
    const buildId = this.selectedBuildId();
    if (!buildId || attempt === this.attemptNumberForLogOnScreenOrNull()) return;
    this.selectedAttemptNumberOrNull.set(attempt);
    this.stopActiveStream();
    this.buildIdWithEndedLogStream.set(null);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { attempt },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
    this.logLoading.set(true);
    void this.fetchInitialLogs(buildId);
  }

  private isLatestAttemptSelected(): boolean {
    const picked = this.selectedAttemptNumberOrNull();
    if (picked === null) return true;
    const attempts = this.selectedBuildAttempts();
    return attempts.length > 0 && attempts.every((attempt) => attempt.number <= picked);
  }

  private loadFailedDependencies(build: BuildItem): void {
    this.failedDependencies.set([]);
    if (build.status !== 'DependencyFailed') return;
    this.evalService.getBuildGraph(build.id).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (graph) => {
        if (this.selectedBuildId() !== build.id) return;
        this.failedDependencies.set(graph.nodes.filter((node) =>
          node.id !== graph.root && !!node.status && buildPhase(node.status as BuildStatus) === 'failure'));
      },
      error: () => {},
    });
  }

  retryLog(): void {
    const buildId = this.selectedBuildId();
    if (!buildId) return;
    this.logLoading.set(true);
    void this.fetchInitialLogs(buildId);
  }

  private async fetchInitialLogs(buildId: string): Promise<void> {
    this.resetLogState();
    const attempt = this.selectedAttemptNumberOrNull();
    const isStale = () => this.selectedBuildId() !== buildId || this.selectedAttemptNumberOrNull() !== attempt;
    this.loadAttempts(buildId);

    const latest = this.isLatestAttemptSelected();
    const selected = this.builds().find(b => b.id === buildId);
    if (latest && selected && ['Queued', 'Building'].includes(selected.status)) {
      void this.startLogStream(buildId);
      this.watchProgress(buildId);
      return;
    }

    const terminal = !latest || (selected && selected.status !== 'Created');
    if (terminal && (await this.fetchChunkedLog(buildId, attempt))) {
      if (!isStale()) this.logLoading.set(false);
      return;
    }

    try {
      const data = await (await fetchBuildLogResponseWithCookiesOrThrow(buildBuildLogApiPath(buildId, attempt))).json();
      if (!isStale() && !data.error && typeof data.message === 'string' && data.message !== '') {
        const lines = splitLogLines(data.message);
        this.logLines.set(lines);
        this.logLineCount.set(lines.length);
        this.trackRateLimit(lines);
      }
    } catch (error) {
      if (isStale()) return;
      this.logRequestError.set(error);
      this.logLoading.set(false);
      return;
    }

    if (!isStale()) this.logLoading.set(false);
  }

  /// A running build reports its prefetch, download and upload transfers: seed
  /// from the build, then follow the build's live channel until its log stream ends.
  private watchProgress(buildId: string): void {
    this.stopProgressWatch();
    const seed = this.evalService.getBuild(buildId).pipe(
      map(b => b.progress ?? null),
      filter(() => this.buildProgress() === null),
    );
    const updates = this.live.connect<LiveEvent>(`/builds/${buildId}/live`).pipe(
      filter(e => e.event === 'build.progress'),
      map((e): BuildProgress => ({
        phase: e.content.phase ?? 'download',
        bytes_done: e.content.bytes_done ?? 0,
        bytes_total: e.content.bytes_total ?? null,
        paths_done: e.content.paths_done ?? 0,
        paths_total: e.content.paths_total ?? null,
      })),
    );
    this.progressSub = merge(seed, updates).subscribe(p => {
      if (this.selectedBuildId() === buildId) this.buildProgress.set(p);
    });
  }

  private stopProgressWatch(): void {
    this.progressSub?.unsubscribe();
    this.progressSub = undefined;
    this.buildProgress.set(null);
  }

  private async startLogStream(buildId: string): Promise<void> {
    this.stopActiveStream();
    const controller = new AbortController();
    this.logStreamAbortController = controller;
    const isCurrent = () => this.logStreamAbortController === controller;

    let response: Response;
    try {
      response = await fetchBuildLogResponseWithCookiesOrThrow(`builds/${buildId}/log`, { method: 'POST', signal: controller.signal });
    } catch (error) {
      if (!isCurrent()) return;
      this.logStreamAbortController = undefined;
      this.stopProgressWatch();
      this.logRequestError.set(error);
      this.logLoading.set(false);
      return;
    }

    try {
      if (response.body && isCurrent()) {
        this.logLoading.set(false);
        if (this.buildIdWithEndedLogStream() === buildId) this.buildIdWithEndedLogStream.set(null);
        const reader = response.body.getReader();
        const textDecoder = new TextDecoder();
        const logDecoder = new BuildLogStreamDecoder();
        const queueLines = (lines: string[]) => {
          if (lines.length === 0) return;
          this.buildLogStreamReconnectAttemptsSinceLastOutput = 0;
          this.pendingLogLines.push(...lines);
          this.startLogDrainTimer();
        };

        while (true) {
          const { done, value } = await reader.read();
          if (!isCurrent()) break;
          if (done) {
            queueLines(logDecoder.takeLastLogLineWithoutNewline());
            break;
          }
          queueLines(logDecoder.appendTextAndReturnCompletedLines(textDecoder.decode(value, { stream: true })));
        }
      }
    } catch {
      // The stream broke off or ended early. The finally block can run the reconnect check.
    } finally {
      if (isCurrent()) {
        this.logStreamAbortController = undefined;
        this.stopProgressWatch();
        this.fetchBuildAndScheduleReconnectIfQueuedOrBuilding(buildId);
      }
    }
  }

  private fetchBuildAndScheduleReconnectIfQueuedOrBuilding(buildId: string): void {
    const scheduleReconnect = () => {
      if (this.selectedBuildId() !== buildId) return;
      this.buildIdWithEndedLogStream.set(buildId);
      const delay = Math.min(1000 * 2 ** this.buildLogStreamReconnectAttemptsSinceLastOutput, 30_000);
      this.buildLogStreamReconnectAttemptsSinceLastOutput++;
      clearTimeout(this.logStreamReconnectTimer);
      this.logStreamReconnectTimer = setTimeout(() => this.reloadLogAndStartStream(), delay);
    };
    this.evalService.getBuild(buildId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (build) => {
        if (isPendingBuildStatus(build.status)) return scheduleReconnect();
        if (this.buildIdWithEndedLogStream() === buildId) this.buildIdWithEndedLogStream.set(null);
      },
      error: scheduleReconnect,
    });
  }

  reloadLogAndStartStream(): void {
    clearTimeout(this.logStreamReconnectTimer);
    this.logStreamReconnectTimer = undefined;
    const buildId = this.selectedBuildId();
    if (buildId) void this.fetchInitialLogs(buildId);
  }

  private stopActiveStream(): void {
    this.logStreamAbortController?.abort();
    this.logStreamAbortController = undefined;
    this.pendingLogLines = [];
    clearTimeout(this.logStreamReconnectTimer);
    this.logStreamReconnectTimer = undefined;
    this.stopLogDrainTimer();
    this.stopProgressWatch();
  }

  private resetLogState(): void {
    this.logRequestError.set(null);
    this.logLines.set([]);
    this.serverLog.set(null);
    this.logLineCount.set(0);
    this.logPaused.set(false);
  }

  private async fetchChunkedLog(buildId: string, attempt: number | null): Promise<boolean> {
    let index: LogChunkIndex;
    try {
      const data = await (await fetchBuildLogResponseWithCookiesOrThrow(buildBuildLogApiPath(buildId, attempt, '/chunks'))).json();
      index = data.message as LogChunkIndex;
    } catch {
      return false;
    }
    if (!index || index.total_chunks === 0 || index.total_lines === 0) return false;
    if (this.selectedBuildId() !== buildId || this.selectedAttemptNumberOrNull() !== attempt) return true;

    this.serverLog.set(buildLog(buildId, index.total_lines, attempt));
    this.logLineCount.set(index.total_lines);
    return true;
  }

  @HostListener('document:keydown', ['$event'])
  onKeydown(event: KeyboardEvent): void {
    const isFind = isFindShortcut(event);

    // Ctrl/Cmd+F never reaches the browser here: it searches the builds or the
    // open log, whichever the pointer is over.
    const logOpen = !!this.selectedBuildId() && this.selectedSection() !== 'messages';
    if (isFind && this.findSearchesBuilds(logOpen)) {
      event.preventDefault();
      this.openSidebarSearch();
      return;
    }

    if (event.key === 'Escape' && this.sidebarSearchOpen()) {
      this.closeSidebarSearch();
      return;
    }

    if (!logOpen) return;
    if (isFind) {
      event.preventDefault();
      this.logViewer?.openSearch();
    } else if (event.key === 'Escape' && this.logViewer?.searchOpen()) {
      this.logViewer.closeSearch();
    }
  }

  /// The pointer decides; keyboard focus only counts while the pointer is over neither region.
  private findSearchesBuilds(logOpen: boolean): boolean {
    if (this.evaluation()?.only_evaluate) return false;
    if (!logOpen) return true;
    return this.pointerOver ? this.pointerOver === 'builds' : this.sidebarFocused;
  }

  openSidebarSearch(): void {
    this.sidebarSearchOpen.set(true);
    setTimeout(() => { (document.querySelector('.sidebar-search input') as HTMLInputElement | null)?.focus(); }, 0);
  }

  closeSidebarSearch(): void {
    this.sidebarSearchOpen.set(false);
    this.sidebarSearchQuery.set('');
  }

  // ── Log drain animation ─────────────────────────────────────────────────────

  // One batch per tick keeps change detection and layout to a few passes a
  // second; the smooth follow-scroll carries the eye across each batch.
  private startLogDrainTimer(): void {
    if (this.logDrainTimer !== undefined) return;
    this.logDrainTimer = setInterval(() => {
      if (this.pendingLogLines.length === 0) return;
      this.appendStreamedLines(this.pendingLogLines.splice(0));
    }, this.DRAIN_MS);
  }

  private stopLogDrainTimer(): void {
    if (this.logDrainTimer !== undefined) {
      clearInterval(this.logDrainTimer);
      this.logDrainTimer = undefined;
    }
  }

  private flushPendingLogs(): void {
    this.stopLogDrainTimer();
    if (this.pendingLogLines.length > 0) {
      this.appendStreamedLines(this.pendingLogLines.splice(0));
    }
  }

  private appendStreamedLines(lines: string[]): void {
    this.logLines.update(shown => shown.concat(lines));
    this.logLineCount.set(this.logLines().length);
    this.trackRateLimit(lines);
  }

  private trackRateLimit(lines: readonly string[]): void {
    const note = lastRateLimitNote(lines);
    if (note) this.logPaused.set(note === 'paused');
  }

  onBuildsViewportScroll(event: Event): void {
    const el = event.target as HTMLElement;
    // Pre-fetch the next page as the list nears the bottom.
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 240) {
      this.loadMoreBuilds();
    }
  }

  isRunningStatus(status: EvaluationStatus): boolean {
    return isRunningEvaluationStatus(status);
  }

  // ── Build actions ───────────────────────────────────────────────────────────

  openBuildMenu(event: MouseEvent, build: BuildItem, menu: MenuComponent): void {
    this.contextBuild.set(build);
    menu.openAt(event);
  }

  toggleBuildMenu(event: MouseEvent, build: BuildItem, menu: MenuComponent): void {
    this.contextBuild.set(build);
    menu.toggle(event);
  }

  /// Saves the build's complete log, which is not what the page holds in memory:
  /// the viewer only ever materializes a window of a chunked log.
  async downloadLog(build: BuildItem): Promise<void> {
    let log: string;
    try {
      log = await firstValueFrom(this.evalService.getBuildLog(build.id));
    } catch (error) {
      this.notify.failure("Couldn't download log", error);
      return;
    }
    if (!log.trim()) {
      this.notify.failure(build.status === 'DependencyFailed'
        ? "Couldn't download log: This build didn't run because dependencies failed."
        : "Couldn't download log: build produced no output", null);
      return;
    }

    const url = URL.createObjectURL(new Blob([log], { type: 'text/plain;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = `${this.buildFileName(build.name)}.log`;
    link.click();
    URL.revokeObjectURL(url);
  }

  downloadSbom(build: BuildItem): void {
    this.evalService.downloadBuildSbom(build.id).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: response => saveResponse(response, `${this.buildFileName(build.name)}-sbom.cdx.json`),
      error: (error: unknown) => this.notify.failure('Failed to download SBOM', error),
    });
  }

    canPrioritize(build: BuildItem): boolean {
    return this.authService.user()?.superuser === true && canPrioritizeBuild(build);
  }

  canAbort(build: BuildItem): boolean {
    const evaluation = this.evaluation();
    return this.triggerAccess().canEdit && !!evaluation && canAbortBuild(evaluation.status, build.status);
  }

  canRetry(build: BuildItem): boolean {
    const evaluation = this.evaluation();
    return this.triggerAccess().canEdit && !!evaluation
      && canRetryBuild(evaluation.status, build.status);
  }

  prioritizeBuild(build: BuildItem): void {
    this.runBuildAction(this.evalService.prioritizeBuild(build.id), build, 'Build prioritized', 'Failed to prioritize build');
  }

  abortBuild(build: BuildItem): void {
    const ev = this.evaluation();
    const target = buildAbortTargetLine(this.buildDisplayName(build.name), ev?.task_display_name || ev?.task_name || this.projectName);
    this.confirm.confirm(abortBuildConfirm(target, () =>this.evalService.abortBuild(build.id)))
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe(() => this.buildActionDone('Build aborted', build));
  }

  retryBuild(build: BuildItem): void {
    this.runBuildAction(this.evalService.retryBuild(build.id), build, 'Build retried', 'Failed to retry build');
  }

  private runBuildAction(request: Observable<string>, build: BuildItem, successSummary: string, failureSummary: string): void {
    request.pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: () => this.buildActionDone(successSummary, build),
      error: (error: unknown) => this.notify.failure(failureSummary, error),
    });
  }

  private buildActionDone(summary: string, build: BuildItem): void {
    this.messageService.add({ severity: 'success', summary, detail: this.buildDisplayName(build.name) });
    this.refreshEvaluation();
  }

  runWaitingExecJob(job: ExecJobSummary): void {
    this.execJobStartingId.set(job.id);
    this.execJobs.run(job.id).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: () => {
        this.execJobStartingId.set(null);
        this.execJobsWaitingWithReason.update((jobs) => jobs.filter((waiting) => waiting.id !== job.id));
        this.notify.success('Exec job added to queue');
      },
      error: (error: unknown) => {
        this.execJobStartingId.set(null);
        this.notify.failure("Couldn't start exec job", error);
      },
    });
  }

  // ── Abort ───────────────────────────────────────────────────────────────────

  requestAbort(event: MouseEvent): void {
    if (event.ctrlKey || event.metaKey) {
      this.abortAtOnce();
      return;
    }
    const ev = this.evaluation();
    const target = evaluationAbortTarget(ev?.task_display_name || ev?.task_name || this.projectName, ev?.commit);
    this.confirm.confirm(abortEvaluationConfirm(target, () => this.abortRequest()))
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe(() => this.evaluationAborted());
  }

  private abortAtOnce(): void {
    if (this.aborting()) return;
    this.aborting.set(true);
    this.abortRequest().pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
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

  private abortRequest(): Observable<string> {
    return this.evalService.abortEvaluation(this.evaluationId);
  }

  private evaluationAborted(): void {
    this.notify.success('Evaluation aborted');
    this.refreshEvaluation();
  }

  // ── Helpers ──────────────────────────────────────────────────────────────────

  selectAdjacentBuild(fromId: string, step: 1 | -1): void {
    const list = this.navigableBuilds();
    const target = list[list.findIndex((b) => b.id === fromId) + step];
    if (!target) return;
    this.selectBuild(target, true);
    const targetId = target.id;
    setTimeout(() => {
      const el = document.querySelector<HTMLElement>(`.build-item[data-build-id="${targetId}"] .sidebar-row-link`);
      el?.scrollIntoView({ block: 'nearest' });
      el?.focus();
    }, 0);
  }

  // <hash>-name-version.drv -> <hash>-name-version (the store file name, without .drv)
  private buildFileName(path: string): string {
    return (path.split('/').pop() ?? path).replace(/\.drv$/, '');
  }

  readonly buildDisplayName = packageNameFromDerivationPath;

  protected splitNameHeadAndTail(name: string): { head: string; tail: string } {
    const cut = Math.max(0, name.length - packageNameVisibleTrailingCharacterCount);
    return { head: name.slice(0, cut), tail: name.slice(cut) };
  }

  protected readonly buildOutcomeLabel = buildOutcomeLabel;
  protected readonly abortedWithoutBuildsTitle = abortedWithoutBuildsTitle;

  getBuildElapsed(build: BuildItem): string {
    return buildDurationTextForEvaluation(build, this.evaluation(), this.clock.currentTimeMilliseconds());
  }

  isRunning(): boolean {
    const status = this.evaluation()?.status;
    return !!status && isRunningEvaluationStatus(status);
  }

  protected readonly links = links;

  readonly commitLabel = commitLabel;
  readonly getEvaluationTriggerLabel = getEvaluationTriggerLabel;

  getStatusLabel(status: string): string {
    if (status === 'Fetching') return 'Fetching';
    if (status === 'EvaluatingFlake' || status === 'EvaluatingDerivation') return 'Evaluating';
    return status;
  }
}
