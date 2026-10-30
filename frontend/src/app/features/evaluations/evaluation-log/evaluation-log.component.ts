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
import { LogViewerComponent, ServerLog, ansiToHtml, isFindShortcut, parseLineFragment, splitLogLines } from '@shared/log';
import { matchesBuildSearch } from './build-search';
import { buildLog } from './build-log';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';
import { Observable, Subscription, merge } from 'rxjs';
import { auditTime, filter, map, share, switchMap } from 'rxjs/operators';
import { EvaluationsService } from '@core/services/evaluations.service';
import { LiveEvent, LiveService } from '@core/services/live.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TasksService } from '@core/services/tasks.service';
import { AccessService, WritableDirective } from '@shared/access';
import { AccessState, accessFromEntity } from '@core/models/access.model';
import {
  BuildProgress,
  EvaluationResponse,
  EvaluationProgress,
  BuildItem,
  EvaluationMessageResponse,
  LogChunkIndex,
  EvaluationStatus,
  BuildWithOutputs,
  WaitingReason,
  TriggerType,
} from '@core/models';
import { AuthService } from '@core/services/auth.service';
import {
  BadgeComponent,
  BadgeSeverity,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MenuComponent,
  MenuItem,
  MessageBannerComponent,
  MessageService,
  SkeletonComponent,
  ToastComponent,
} from '@gradient/ui/ui';
import { BuildProgressComponent, EvalStatusBadgeComponent, InputFetchListComponent, ThunkProgressComponent } from '@shared/ui';
import { buildDuration, buildPhaseFinished, canAbortBuild, canPrioritizeBuild, canRetryBuild, commitLabel, evaluationDuration, formatEvaluationDuration, isRunningEvaluationStatus, phaseProgress, thunkProgress, formatWaitingReason, waitingTitle } from '@shared/evaluation';
import { environment } from '@environments/environment';
import { saveResponse } from '@shared/download';


@Component({
  selector: 'app-evaluation-log',
  standalone: true,
  imports: [CommonModule, RouterModule, LoadingSpinnerComponent, EmptyStateComponent, ButtonComponent, DialogComponent, IconComponent, BadgeComponent, SkeletonComponent, BuildProgressComponent, EvalStatusBadgeComponent, InputDirective, InputFetchListComponent, LogViewerComponent, MenuComponent, MessageBannerComponent, ThunkProgressComponent, ToastComponent, WritableDirective],
  providers: [MessageService],
  templateUrl: './evaluation-log.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: [
    '../../../shared/log/log-page.scss',
    '../../../shared/log/log-sidebar.scss',
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
  protected authService = inject(AuthService);
  private sanitizer = inject(DomSanitizer);
  private destroyRef = inject(DestroyRef);

  @ViewChild(LogViewerComponent) logViewer?: LogViewerComponent;

  loading = signal(true);
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
  messages = signal<(EvaluationMessageResponse & { renderedHtml: SafeHtml })[]>([]);
  selectedBuildId = signal<string | null>(null);
  selectedSection = signal<'messages' | null>(null);
  buildsOpen = signal(false);
  logLineCount = signal(0);
  logLoading = signal(true);
  aborting = signal(false);
  abortConfirmOpen = signal(false);
  // The log page is reachable without the task route's access resolver, so the
  // owning task is asked directly: a viewer must not be offered an abort.
  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));
  duration = signal('0s');
  totalBuildsCount = signal(0);
  activeBuildsCount = signal(0);
  private tick = signal(0);

  projectName = '';
  projectAndTaskCrumbs = computed(() => {
    const task = this.evaluation()?.task_name;
    const trail = task ? this.crumbs.task(this.projectName, task) : this.crumbs.project(this.projectName);

    return trail.slice(1);
  });
  evaluationId = '';
  private initialBuildId: string | null = null;
  // #489: when set, the build list is scoped to this build's package closure.
  scopeBuildId = signal<string | null>(null);
  private initialShowEval = false;
  private fetchingInitialBuild = false;

  // Derived from backend totals - accurate regardless of how many builds are loaded.
  completedBuildsCount = computed(() => this.totalBuildsCount() - this.activeBuildsCount());

  queuedCount = computed(() =>
    this.builds().filter(b => b.status === 'Queued').length
  );

  buildingCount = computed(() =>
    this.builds().filter(b => b.status === 'Building').length
  );

  selectedBuild = computed(() =>
    this.builds().find(b => b.id === this.selectedBuildId()) ?? null
  );

  visibleBuilds = signal<BuildItem[]>([]);

  // The build the sidebar's menu is acting on, opened by right-click or the
  // row's three dots.
  contextBuild = signal<BuildItem | null>(null);

  buildMenuModel = computed<MenuItem[]>(() => {
    const build = this.contextBuild();
    if (!build) return [];

    const inEvaluation = { evalId: this.evaluationId };
    const job = build.dispatched_job;
    return [
      {
        label: 'Graph',
        icon: 'account_tree',
        routerLink: ['/project', this.projectName, 'graph', build.id],
        queryParams: inEvaluation,
      },
      {
        label: 'Show Job',
        icon: 'work',
        disabled: !job,
        routerLink: job ? ['/board', 'jobs', job] : undefined,
      },
      {
        label: 'Artefacts',
        icon: 'inventory_2',
        disabled: !build.has_artefacts,
        routerLink: build.has_artefacts ? ['/project', this.projectName, 'artefacts', build.id] : undefined,
        queryParams: inEvaluation,
      },
      { separator: true },
      {
        label: 'Download Log',
        icon: 'download',
        disabled: ['Created', 'Queued'].includes(build.status),
        command: () => void this.downloadLog(build),
      },
      {
        label: 'SBOM',
        icon: 'description',
        disabled: !['Completed', 'Substituted'].includes(build.status),
        command: () => this.downloadSbom(build),
      },
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
  private durationInterval?: ReturnType<typeof setInterval>;
  private activeStreamReader?: ReadableStreamDefaultReader<Uint8Array>;
  private streamingBuildId?: string;
  private autoFollowBuilding = false;
  private userPickedBuild = false;
  private isInitialBuildsLoad = true;
  private pendingBuilds: BuildItem[] = [];
  private buildRevealTimer?: ReturnType<typeof setInterval>;
  private pendingLogLines: string[] = [];
  private logDrainTimer?: ReturnType<typeof setInterval>;
  private readonly PAGE_SIZE = 1000;
  private totalBuilds = 0;
  private loadingMore = false;

  logLines = signal<readonly string[]>([]);
  serverLog = signal<ServerLog | null>(null);
  deepLinkLine = signal<number | null>(null);
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
    this.scopeBuildId.set(query.get('build'));
    this.initialShowEval = query.get('eval') !== null;
    this.initialBuildId = this.initialShowEval ? null : query.get('selected') ?? query.get('build');
    this.deepLinkLine.set(parseLineFragment(this.route.snapshot.fragment));
    if (!this.evaluationId) {
      this.loading.set(false);
      return;
    }
    this.loadEvaluation();
  }

  ngOnDestroy(): void {
    document.documentElement.style.overflow = '';
    document.body.style.overflow = '';
    this.stopLiveUpdates();
    this.stopDurationTimer();
    this.stopActiveStream();
    this.stopBuildRevealTimer();
  }

  loadEvaluation(): void {
    this.loading.set(true);
    this.evalService.getEvaluation(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (evaluation) => {
        this.rememberTaskName(evaluation);
        this.evaluation.set(evaluation);
        this.loading.set(false);
        this.loadAccess(evaluation.task_name);
        if (this.initialBuildId) {
          this.resolveInitialBuild(() => this.loadBuilds());
        } else {
          this.loadBuilds();
        }
        this.loadMessages();
        this.startDurationTimer(evaluation);
        this.startLiveUpdates(evaluation.status);
      },
      error: () => this.loading.set(false),
    });
  }

  private refreshEvaluation(): void {
    this.evalService.getEvaluation(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe(evaluation => {
      this.evaluation.set(evaluation);
      this.loadBuilds();
      if (this.isRunningStatus(evaluation.status) && !this.liveSub) {
        this.startDurationTimer(evaluation);
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
        next: (task) => this.access.set(accessFromEntity(task)),
        error: () => {},
      });
  }

  loadMessages(): void {
    this.evalService.getEvaluationMessages(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (msgs) => this.messages.set(
        msgs.map(m => ({
          ...m,
          renderedHtml: this.sanitizer.bypassSecurityTrustHtml(ansiToHtml(m.message)),
        }))
      ),
    });
  }

  private readonly buildStatusOrder: Record<string, number> = {
    building: 0,
    failed: 1,
    aborted: 2,
    queued: 3,
    completed: 4,
    substituted: 4,
    dependencyfailed: 5,
    skipped: 6,
  };

  /// Sidebar sections, in display order. Failures sort above queued so they stay
  /// visible. `Completed` absorbs `Substituted`, `Skipped` absorbs `DependencyFailed`.
  private readonly buildGroups: { key: string; label: string; members: string[] }[] = [
    { key: 'building', label: 'Building', members: ['building'] },
    { key: 'failed', label: 'Failed', members: ['failed'] },
    { key: 'aborted', label: 'Aborted', members: ['aborted'] },
    { key: 'queued', label: 'Queued', members: ['queued'] },
    { key: 'completed', label: 'Completed', members: ['completed', 'substituted'] },
    { key: 'skipped', label: 'Skipped', members: ['dependencyfailed', 'skipped'] },
  ];

  collapsedGroups = signal<ReadonlySet<string>>(new Set());

  /// `visibleBuilds` bucketed into the status sections above (only non-empty
  /// ones), in the flat status-sorted order keyboard navigation walks.
  groupedBuilds = computed(() => {
    const indexByGroup = new Map<string, number>();
    this.buildGroups.forEach((g, i) => g.members.forEach((m) => indexByGroup.set(m, i)));
    const buckets: BuildItem[][] = this.buildGroups.map(() => []);
    for (const build of this.visibleBuilds()) {
      if (!matchesBuildSearch(build.name, this.sidebarSearchQuery())) continue;
      const gi = indexByGroup.get(this.statusClass(build.status));
      if (gi !== undefined) buckets[gi].push(build);
    }
    const collapsed = this.collapsedGroups();
    return this.buildGroups
      .map((g, i) => ({ key: g.key, label: g.label, builds: buckets[i], collapsed: collapsed.has(g.key) }))
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

  /// Maps every BuildStatus variant onto the canonical token used by the SCSS
  /// status classes (status-failed, status-completed, …). The three failed
  /// reasons collapse onto `failed` so the red dot/badge renders for all of them.
  /// The badge severity a build status reads as.
  statusSeverity(status: string | null | undefined): BadgeSeverity {
    switch (this.statusClass(status)) {
      case 'completed':
      case 'substituted': return 'success';
      case 'building': return 'info';
      case 'queued': return 'warning';
      case 'failed':
      case 'dependencyfailed': return 'danger';
      default: return 'neutral';
    }
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
      const oa = this.buildStatusOrder[this.statusClass(a.status)] ?? 99;
      const ob = this.buildStatusOrder[this.statusClass(b.status)] ?? 99;
      if (oa !== ob) return oa - ob;
      if (a.depth !== b.depth) return a.depth - b.depth;
      return this.buildDisplayName(a.name).localeCompare(this.buildDisplayName(b.name));
    });
  }

  loadBuilds(): void {
    // Initial load uses PAGE_SIZE for speed. Subsequent polls fetch ALL builds so that
    // builds which changed sort rank (e.g. Building -> Completed moves from rank 0 to rank 4)
    // are never left in `beyondFirstPage` with a stale status.
    const limit = this.isInitialBuildsLoad
      ? this.PAGE_SIZE
      : Math.max(this.PAGE_SIZE, this.totalBuilds, this.activeBuildsCount());
    this.evalService.getBuilds(this.evaluationId, limit, 0, this.scopeBuildId() ?? undefined).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (result) => {
        this.totalBuilds = result.total;
        this.totalBuildsCount.set(result.total);
        this.activeBuildsCount.set(result.active_count);
        const prevSelected = this.selectedBuild();

        // Merge: keep any already-loaded builds beyond the first page,
        // replace the first page with fresh data.
        const freshIds = new Set(result.builds.map(b => b.id));
        const beyondFirstPage = this.builds().filter(b => !freshIds.has(b.id));
        // Only keep beyond-page builds that aren't in the fresh result
        // (they may have changed status and moved into the first page)
        this.builds.set(this.sortBuilds([...result.builds, ...beyondFirstPage]));

        const newSelected = this.selectedBuild();
        const isEvaluating = this.evaluation()?.status === 'Fetching' || this.evaluation()?.status === 'EvaluatingFlake' || this.evaluation()?.status === 'EvaluatingDerivation';

        // ── Build list visibility ───────────────────────────────────────────
        if (this.isInitialBuildsLoad) {
          // First load: show everything we have immediately
          this.visibleBuilds.set(this.builds());
          this.isInitialBuildsLoad = false;
          const evalStatus = this.evaluation()?.status;
          const failedWithNoBuilds = result.total === 0 && evalStatus === 'Failed';
          if (this.initialShowEval || failedWithNoBuilds) {
            // Open the evaluation messages view directly (explicit request via ?eval,
            // or implicit fallback when there is nothing to build).
            this.initialShowEval = false;
            this.selectEvaluationSection();
          } else if (!this.initialBuildId && !this.selectedBuildId()) {
            // Auto-select first Building build when no explicit build was requested
            const firstBuilding = this.builds().find(b => b.status === 'Building');
            if (firstBuilding) {
              this.selectBuild(firstBuilding); // isUserAction=false -> auto-follow mode
            } else {
              this.autoFollowBuilding = true; // wait for first build to start
            }
          }
        } else if (!isEvaluating) {
          // Evaluation is no longer Evaluating: flush all builds immediately
          this.flushPendingBuilds();
        } else {
          // Still evaluating: incrementally reveal newly added builds
          const knownIds = new Set([
            ...this.visibleBuilds().map(b => b.id),
            ...this.pendingBuilds.map(b => b.id),
          ]);

          const newBuilds = this.builds().filter(b => !knownIds.has(b.id));
          if (newBuilds.length > 0) {
            this.pendingBuilds.unshift(...newBuilds);
            this.startBuildRevealTimer();
          }

          // Refresh statuses & order of already-visible builds
          this.visibleBuilds.update(vbs =>
            this.sortBuilds(vbs.map(vb => this.builds().find(b => b.id === vb.id) ?? vb))
          );
        }

        // ── Log transitions ─────────────────────────────────────────────────

        // Queued -> Building: reload logs for the newly started build
        if (prevSelected?.status === 'Queued' && newSelected?.status === 'Building') {
          this.pendingLogLines = [];
          this.logLoading.set(true);
          this.fetchInitialLogs(newSelected.id);
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
        if (this.builds().length < result.active_count && !this.loadingMore) {
          this.doLoadMore();
        }
      },
    });
  }

  /** Triggered by scroll proximity - pre-fetches next page before user hits bottom. */
  loadMoreBuilds(): void {
    if (this.loadingMore || this.builds().length >= this.totalBuilds) return;
    this.doLoadMore();
  }

  private doLoadMore(): void {
    if (this.loadingMore || this.builds().length >= this.totalBuilds) return;
    this.loadingMore = true;
    const offset = this.builds().length;

    this.evalService.getBuilds(this.evaluationId, this.PAGE_SIZE, offset, this.scopeBuildId() ?? undefined).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (result) => {
        // Do NOT update totalBuildsCount / activeBuildsCount here - those metric signals
        // are owned exclusively by loadBuilds() to avoid jumps from concurrent responses.
        this.totalBuilds = result.total;
        if (result.builds.length > 0) {
          const existingIds = new Set(this.builds().map(b => b.id));
          const newBuilds = result.builds.filter(b => !existingIds.has(b.id));
          if (newBuilds.length > 0) {
            this.builds.update(current => this.sortBuilds([...current, ...newBuilds]));
            this.visibleBuilds.update(current => this.sortBuilds([...current, ...newBuilds]));
          }
        }
        this.loadingMore = false;

        // A build paged in here may be the one we deep-linked to.
        this.resolveInitialBuild();

        // Continue fetching if there are still active builds not yet in memory
        if (this.builds().length < result.active_count) {
          this.doLoadMore();
        }
      },
      error: () => {
        this.loadingMore = false;
      },
    });
  }

  startLiveUpdates(status: EvaluationStatus): void {
    this.stopLiveUpdates();
    if (!this.isRunningStatus(status)) return;

    const frames = this.live.connect<LiveEvent>(`/evals/${this.evaluationId}/live`).pipe(share());
    this.liveSub = frames
      .pipe(filter(e => e.event === 'evaluation.activity'))
      .subscribe(e => this.liveProgress.set(e.content.progress ?? null));
    this.liveSub.add(
      frames
        .pipe(
          filter(e => e.event !== 'evaluation.activity'),
          auditTime(300),
          switchMap(() => this.evalService.getEvaluation(this.evaluationId)),
        )
        .subscribe({
          next: (evaluation) => {
            this.evaluation.set(evaluation);
            this.loadBuilds();
            if (!this.isRunningStatus(evaluation.status)) {
              this.stopLiveUpdates();
              this.updateDuration(evaluation);
              this.stopDurationTimer();
              this.loadBuilds(); // final update
              this.loadMessages(); // pick up any messages recorded during eval
            }
          },
        }),
    );
  }

  stopLiveUpdates(): void {
    this.liveSub?.unsubscribe();
    this.liveSub = undefined;
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
      queryParams: { selected: null, eval: 1 },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
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
          // `?build=` also scopes the list to this build's closure, so it is the
          // root of everything the API returns.
          depth: 0,
        };
        this.initialBuildId = null;
        this.builds.update(cur => this.sortBuilds([item, ...cur]));
        this.visibleBuilds.update(cur => this.sortBuilds([item, ...cur]));
        this.selectBuild(item, true);
        settled?.();
      },
      error: () => {
        this.fetchingInitialBuild = false;
        settled?.();
      },
    });
  }

  selectBuild(build: BuildItem, isUserAction = false): void {
    this.buildsOpen.set(false);
    this.selectedSection.set(null);
    if (this.selectedBuildId() === build.id) return;

    this.userPickedBuild = isUserAction;
    this.autoFollowBuilding = false;
    this.stopActiveStream();
    this.resetLogState();
    this.selectedBuildId.set(build.id);

    const scope = this.scopeBuildId();
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { build: scope ?? build.id, selected: scope && scope !== build.id ? build.id : null, eval: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });

    this.logLoading.set(true);
    this.fetchInitialLogs(build.id);
  }

  private async fetchInitialLogs(buildId: string): Promise<void> {
    this.resetLogState();
    const selected = this.builds().find(b => b.id === buildId);
    const terminal = selected && !['Queued', 'Building', 'Created'].includes(selected.status);
    if (terminal && (await this.fetchChunkedLog(buildId))) {
      this.logLoading.set(false);
      return;
    }

    try {
      const response = await fetch(`${environment.apiUrl}/builds/${buildId}/log`, {
        method: 'GET',
        credentials: 'include',
      });

      if (response.ok) {
        const data = await response.json();
        if (!data.error && typeof data.message === 'string' && data.message !== '') {
          const lines = splitLogLines(data.message);
          this.logLines.set(lines);
          this.logLineCount.set(lines.length);
        }
      }
    } catch {
      // ignore
    }

    this.logLoading.set(false);

    // If this build is still running, start streaming
    const build = this.builds().find(b => b.id === buildId);
    const isBuilding = build && ['Queued', 'Building'].includes(build.status);
    if (isBuilding) {
      this.startLogStream(buildId);
      this.watchProgress(buildId);
    }
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
    if (this.streamingBuildId === buildId) return;
    this.stopActiveStream();
    this.streamingBuildId = buildId;

    try {
      const response = await fetch(`${environment.apiUrl}/builds/${buildId}/log`, {
        method: 'POST',
        credentials: 'include',
      });

      if (!response.ok || !response.body) return;

      this.activeStreamReader = response.body.getReader();
      const decoder = new TextDecoder();

      while (true) {
        const { done, value } = await this.activeStreamReader.read();
        if (done) break;

        // Stop processing if another build was selected
        if (this.selectedBuildId() !== buildId) break;

        const chunk = decoder.decode(value, { stream: true });
        if (chunk.trim()) {
          const newLines = this.parseLogContent(chunk);
          this.pendingLogLines.push(...newLines);
          this.startLogDrainTimer();
        }
      }
    } catch {
      // stream ended or aborted
    } finally {
      this.activeStreamReader = undefined;
      this.streamingBuildId = undefined;
      if (this.selectedBuildId() === buildId) this.stopProgressWatch();
    }
  }

  private stopActiveStream(): void {
    try { this.activeStreamReader?.cancel(); } catch { /* ignore */ }
    this.activeStreamReader = undefined;
    this.streamingBuildId = undefined;
    this.pendingLogLines = [];
    this.stopLogDrainTimer();
    this.stopProgressWatch();
  }

  private resetLogState(): void {
    if (this.logLineCount() > 0) this.deepLinkLine.set(null);
    this.logLines.set([]);
    this.serverLog.set(null);
    this.logLineCount.set(0);
  }

  private async fetchChunkedLog(buildId: string): Promise<boolean> {
    let index: LogChunkIndex;
    try {
      const res = await fetch(`${environment.apiUrl}/builds/${buildId}/log/chunks`, {
        method: 'GET',
        credentials: 'include',
      });
      if (!res.ok) return false;
      const data = await res.json();
      index = data.message as LogChunkIndex;
    } catch {
      return false;
    }
    if (!index || index.total_chunks === 0 || index.total_lines === 0) return false;

    this.serverLog.set(buildLog(buildId, index.total_lines));
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

  // ── Build reveal animation ──────────────────────────────────────────────────

  private startBuildRevealTimer(): void {
    if (this.buildRevealTimer !== undefined) return;
    this.buildRevealTimer = setInterval(() => {
      if (this.pendingBuilds.length === 0) {
        this.stopBuildRevealTimer();
        return;
      }
      const next = this.pendingBuilds.shift()!;
      this.visibleBuilds.update(vbs => vbs.some(b => b.id === next.id) ? vbs : [next, ...vbs]);
    }, 10);
  }

  private stopBuildRevealTimer(): void {
    if (this.buildRevealTimer !== undefined) {
      clearInterval(this.buildRevealTimer);
      this.buildRevealTimer = undefined;
    }
  }

  private flushPendingBuilds(): void {
    this.stopBuildRevealTimer();
    this.pendingBuilds = [];
    this.visibleBuilds.set(this.builds());
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
  }

  // ── Log parsing ─────────────────────────────────────────────────────────────

  private parseLogContent(raw: string): string[] {
    return raw
      .split('\n')
      .filter(line => {
        const t = line.trim();
        // Keep non-empty transport lines; discard sentinel '""' and bare empty lines
        return t !== '' && t !== '""' && t !== "''";
      })
      .flatMap(line => {
        // Try JSON.parse first to correctly decode \u001b and other escape sequences
        if (line.length >= 2 && line.startsWith('"') && line.endsWith('"')) {
          try {
            line = JSON.parse(line) as string;
          } catch {
            line = line.slice(1, -1);
          }
        }
        // Handle remaining literal escape sequences (non-JSON streams)
        const decoded = line
          .replace(/\\u001b/g, '\u001b')
          .replace(/\\n/g, '\n')
          .replace(/\\t/g, '\t');
        // Strip a single trailing newline so split('\n') doesn't produce a
        // spurious empty element that renders as a blank line between chunks.
        const trimmed = decoded.endsWith('\n') ? decoded.slice(0, -1) : decoded;
        return trimmed.split('\n');
      });
  }

  onBuildsViewportScroll(event: Event): void {
    const el = event.target as HTMLElement;
    // Pre-fetch the next page as the list nears the bottom.
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 240) {
      this.loadMoreBuilds();
    }
  }

  // ── Duration ─────────────────────────────────────────────────────────────────

  private startDurationTimer(evaluation: EvaluationResponse): void {
    this.stopDurationTimer();
    this.updateDuration(evaluation);

    if (!this.isRunningStatus(evaluation.status)) return;

    this.durationInterval = setInterval(() => {
      const ev = this.evaluation();
      if (ev) this.updateDuration(ev);
      this.tick.update(t => t + 1);
    }, 1000);
  }

  private stopDurationTimer(): void {
    if (this.durationInterval !== undefined) {
      clearInterval(this.durationInterval);
      this.durationInterval = undefined;
    }
  }

  private updateDuration(evaluation: EvaluationResponse): void {
    this.duration.set(formatEvaluationDuration(evaluationDuration(evaluation, Date.now())));
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
    event.stopPropagation();
    this.contextBuild.set(build);
    menu.toggle(event);
  }

  /// Saves the build's complete log, which is not what the page holds in memory:
  /// the viewer only ever materializes a window of a chunked log.
  async downloadLog(build: BuildItem): Promise<void> {
    let log: string;
    try {
      const response = await fetch(`${environment.apiUrl}/builds/${build.id}/log`, { credentials: 'include' });
      if (!response.ok) return;
      const data = await response.json();
      if (data.error || typeof data.message !== 'string') return;
      log = data.message;
    } catch {
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
      error: (error: Error) => this.messageService.add({
        severity: 'error',
        summary: 'SBOM download failed',
        detail: error?.message,
      }),
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
      && canRetryBuild({ status: evaluation.status, replaced: !!evaluation.next }, build.status);
  }

  prioritizeBuild(build: BuildItem): void {
    this.runBuildAction(this.evalService.prioritizeBuild(build.id), build, 'Build prioritized', 'Prioritize failed', 'Failed to prioritize build.');
  }

  abortBuild(build: BuildItem): void {
    this.runBuildAction(this.evalService.abortBuild(build.id), build, 'Build aborted', 'Abort failed', 'Failed to abort build.');
  }

  retryBuild(build: BuildItem): void {
    this.runBuildAction(this.evalService.retryBuild(build.id), build, 'Build retried', 'Retry failed', 'Failed to retry build.');
  }

  private runBuildAction(request: Observable<string>, build: BuildItem, successSummary: string, failureSummary: string, failureDetail: string): void {
    request.pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: () => {
        this.messageService.add({ severity: 'success', summary: successSummary, detail: this.buildDisplayName(build.name) });
        this.refreshEvaluation();
      },
      error: (error: Error) => this.messageService.add({
        severity: 'error',
        summary: failureSummary,
        detail: error?.message || failureDetail,
      }),
    });
  }

  // ── Abort ───────────────────────────────────────────────────────────────────

  requestAbort(event: MouseEvent): void {
    if (event.ctrlKey || event.metaKey) this.abortEvaluation();
    else this.abortConfirmOpen.set(true);
  }

  abortEvaluation(): void {
    this.aborting.set(true);
    this.evalService.abortEvaluation(this.evaluationId).pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: () => {
        this.aborting.set(false);
        this.abortConfirmOpen.set(false);
        this.loadEvaluation();
      },
      error: () => this.aborting.set(false),
    });
  }

  // ── Helpers ──────────────────────────────────────────────────────────────────

  selectAdjacentBuild(fromId: string, step: 1 | -1): void {
    const list = this.navigableBuilds();
    const target = list[list.findIndex((b) => b.id === fromId) + step];
    if (!target) return;
    this.selectBuild(target, true);
    const targetId = target.id;
    setTimeout(() => {
      const el = document.querySelector<HTMLElement>(`.build-item[data-build-id="${targetId}"]`);
      el?.scrollIntoView({ block: 'nearest' });
      el?.focus();
    }, 0);
  }

  // <hash>-name-version.drv -> <hash>-name-version (the store file name, without .drv)
  private buildFileName(path: string): string {
    return (path.split('/').pop() ?? path).replace(/\.drv$/, '');
  }

  buildDisplayName(path: string): string {
    // <hash>-name-version.drv -> name-version (strip leading hash, drop .drv)
    const filename = path.split('/').pop() ?? path;
    return filename.replace(/^[^-]+-/, '').replace(/\.drv$/, '');
  }

  getBuildElapsed(build: BuildItem): string {
    if (build.status !== 'Completed' && build.status !== 'Building') return '';
    this.tick(); // reactive dependency - re-evaluated every second
    const ms = buildDuration(build, Date.now());
    return ms == null ? '' : this.formatMs(ms);
  }

  private formatMs(ms: number): string {
    const totalSecs = Math.floor(ms / 1000);
    const h = Math.floor(totalSecs / 3600);
    const m = Math.floor((totalSecs % 3600) / 60);
    const s = totalSecs % 60;
    return h > 0
      ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
      : `${m}:${String(s).padStart(2, '0')}`;
  }

  isRunning(): boolean {
    const s = this.evaluation()?.status;
    return s === 'Queued' || s === 'Fetching' || s === 'EvaluatingFlake' || s === 'EvaluatingDerivation' || s === 'Building' || s === 'Waiting';
  }

  readonly formatWaitingReason = formatWaitingReason;
  readonly waitingTitle = waitingTitle;

  isWorkersWaiting(reason: WaitingReason | undefined): boolean {
    return reason?.kind === 'workers';
  }

  readonly commitLabel = commitLabel;

  getTriggerLabel(type: TriggerType | null): string {
    switch (type) {
      case 'polling': return 'Polling';
      case 'reporter_push': return 'Push';
      case 'reporter_pull_request': return 'PR';
      case 'time': return 'Schedule';
      default: return 'Manual';
    }
  }

  getStatusLabel(status: string): string {
    if (status === 'Fetching') return 'Fetching';
    if (status === 'EvaluatingFlake' || status === 'EvaluatingDerivation') return 'Evaluating';
    return status;
  }
}
