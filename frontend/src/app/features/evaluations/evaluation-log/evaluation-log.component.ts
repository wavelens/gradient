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
  ElementRef,
  ChangeDetectorRef,
  HostListener,
} from '@angular/core';
import {
  LogChunkIndex,
  LogSearchHit,
  parseLineFragment,
  windowAround,
} from './log-window';
import { matchesBuildSearch } from './build-search';
import { isTypingTarget } from './keyboard';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';
import { Subscription } from 'rxjs';
import { auditTime, switchMap } from 'rxjs/operators';
import { EvaluationsService, BuildItem, BuildWithOutputs } from '@core/services/evaluations.service';
import { LiveService } from '@core/services/live.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { Evaluation, EvaluationMessage, EvaluationStatus, WaitingReason, TriggerType } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { formatEvaluationDuration, isRunningEvaluationStatus, parseUtcTimestamp } from '@shared/evaluation';
import { ButtonModule } from 'primeng/button';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-evaluation-log',
  standalone: true,
  imports: [CommonModule, RouterModule, LoadingSpinnerComponent, ButtonModule],
  templateUrl: './evaluation-log.component.html',
  styleUrls: [
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
  private orgsService = inject(OrganizationsService);
  protected authService = inject(AuthService);
  private sanitizer = inject(DomSanitizer);
  private cdr = inject(ChangeDetectorRef);

  @ViewChild('logContainer') logContainerRef?: ElementRef<HTMLDivElement>;

  loading = signal(true);
  evaluation = signal<Evaluation | null>(null);
  builds = signal<BuildItem[]>([]);
  messages = signal<(EvaluationMessage & { renderedHtml: SafeHtml })[]>([]);
  selectedBuildId = signal<string | null>(null);
  selectedSection = signal<'messages' | null>(null);
  logHtml = signal<SafeHtml>('');
  logLineCount = signal(0);
  logLoading = signal(true);
  aborting = signal(false);
  autoScroll = signal(true);
  showScrollBtn = signal(false);
  duration = signal('0s');
  totalBuildsCount = signal(0);
  activeBuildsCount = signal(0);
  private tick = signal(0);

  orgName = '';
  orgDisplayName = signal('');
  evaluationId = '';
  private initialBuildId: string | null = null;
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

  errorMessages = computed(() => this.messages().filter(m => m.level === 'Error'));
  warningMessages = computed(() => this.messages().filter(m => m.level === 'Warning'));

  private liveSub?: Subscription;
  private durationInterval?: ReturnType<typeof setInterval>;
  private activeStreamReader?: ReadableStreamDefaultReader<Uint8Array>;
  private streamingBuildId?: string;
  private logLines: string[] = [];
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

  // ── Chunked log viewing (completed builds) ──────────────────────────────────
  chunkedMode = signal(false);
  windowLines = signal<{ n: number; html: SafeHtml }[]>([]);
  topSpacerPx = signal(0);
  bottomSpacerPx = signal(0);
  highlightLine = signal<number | null>(null);
  searchOpen = signal(false);
  searchQuery = signal('');
  sidebarSearchOpen = signal(false);
  sidebarSearchQuery = signal('');
  private sidebarFocused = false;
  searchHits = signal<LogSearchHit[]>([]);
  searchTotal = signal(0);
  currentHit = signal(-1);
  searchLoading = signal(false);
  private searchDebounceTimer?: ReturnType<typeof setTimeout>;
  private searchSeq = 0;
  private readonly MIN_SEARCH_LEN = 3;
  private readonly SEARCH_DEBOUNCE_MS = 300;
  private chunkIndex: LogChunkIndex | null = null;
  private windowStart = 1;
  private windowText: string[] = [];
  private loadingWindow = false;
  private pendingDeepLink: number | null = null;
  private readonly LINE_PX = 18;
  private readonly WINDOW_PAGE = 800;
  private readonly MAX_WINDOW = 4000;

  ngOnInit(): void {
    document.documentElement.style.overflow = 'hidden';
    document.body.style.overflow = 'hidden';
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.evaluationId = this.route.snapshot.paramMap.get('evaluationId') || '';
    this.initialBuildId = this.route.snapshot.queryParamMap.get('build');
    this.initialShowEval = this.route.snapshot.queryParamMap.get('eval') !== null;
    this.pendingDeepLink = parseLineFragment(this.route.snapshot.fragment);
    if (!this.evaluationId) {
      this.loading.set(false);
      return;
    }
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.loadEvaluation();
  }

  ngOnDestroy(): void {
    document.documentElement.style.overflow = '';
    document.body.style.overflow = '';
    this.stopLiveUpdates();
    this.stopDurationTimer();
    this.stopActiveStream();
    this.stopBuildRevealTimer();
    if (this.searchDebounceTimer !== undefined) clearTimeout(this.searchDebounceTimer);
  }

  loadEvaluation(): void {
    this.loading.set(true);
    this.evalService.getEvaluation(this.evaluationId).subscribe({
      next: (evaluation) => {
        this.evaluation.set(evaluation);
        this.loading.set(false);
        this.loadBuilds();
        this.loadMessages();
        this.startDurationTimer(evaluation);
        this.startLiveUpdates(evaluation.status);
      },
      error: () => this.loading.set(false),
    });
  }

  loadMessages(): void {
    this.evalService.getEvaluationMessages(this.evaluationId).subscribe({
      next: (msgs) => this.messages.set(
        msgs.map(m => ({
          ...m,
          renderedHtml: this.sanitizer.bypassSecurityTrustHtml(this.convertAnsiToHtml(m.message)),
        }))
      ),
    });
  }

  private readonly buildStatusOrder: Record<string, number> = {
    building: 0,
    queued: 1,
    aborted: 2,
    failed: 3,
    dependencyfailed: 3,
    completed: 4,
    substituted: 4,
  };

  /// Sidebar sections, in display order. `Completed` absorbs `Substituted`,
  /// `Failed` absorbs `DependencyFailed` - matching the dot colours.
  private readonly buildGroups: { key: string; label: string; members: string[] }[] = [
    { key: 'building', label: 'Building', members: ['building'] },
    { key: 'queued', label: 'Queued', members: ['queued'] },
    { key: 'aborted', label: 'Aborted', members: ['aborted'] },
    { key: 'failed', label: 'Failed', members: ['failed', 'dependencyfailed'] },
    { key: 'completed', label: 'Completed', members: ['completed', 'substituted'] },
  ];

  /// `visibleBuilds` bucketed into the status sections above (only non-empty
  /// ones). Each entry keeps its index in `visibleBuilds` so keyboard
  /// navigation still walks the flat, status-sorted order across sections.
  groupedBuilds = computed(() => {
    const indexByGroup = new Map<string, number>();
    this.buildGroups.forEach((g, i) => g.members.forEach((m) => indexByGroup.set(m, i)));
    const buckets: { build: BuildItem; index: number }[][] = this.buildGroups.map(() => []);
    this.visibleBuilds().forEach((build, index) => {
      if (!matchesBuildSearch(build.name, this.sidebarSearchQuery())) return;
      const gi = indexByGroup.get(this.statusClass(build.status));
      if (gi !== undefined) buckets[gi].push({ build, index });
    });
    return this.buildGroups
      .map((g, i) => ({ key: g.key, label: g.label, builds: buckets[i] }))
      .filter((g) => g.builds.length > 0);
  });

  /// Maps every BuildStatus variant onto the canonical token used by the SCSS
  /// status classes (status-failed, status-completed, …). The three failed
  /// reasons collapse onto `failed` so the red dot/badge renders for all of them.
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

    return [...byKey.values()].sort((a, b) => {
      const oa = this.buildStatusOrder[this.statusClass(a.status)] ?? 99;
      const ob = this.buildStatusOrder[this.statusClass(b.status)] ?? 99;
      if (oa !== ob) return oa - ob;
      return this.buildDisplayName(a.name).localeCompare(this.buildDisplayName(b.name));
    });
  }

  loadBuilds(): void {
    // Initial load uses PAGE_SIZE for speed. Subsequent polls fetch ALL builds so that
    // builds which changed sort rank (e.g. Building → Completed moves from rank 0 to rank 4)
    // are never left in `beyondFirstPage` with a stale status.
    const limit = this.isInitialBuildsLoad
      ? this.PAGE_SIZE
      : Math.max(this.PAGE_SIZE, this.totalBuilds, this.activeBuildsCount());
    this.evalService.getBuilds(this.evaluationId, limit, 0).subscribe({
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
          } else if (!this.initialBuildId) {
            // Auto-select first Building build when no explicit build was requested
            const firstBuilding = this.builds().find(b => b.status === 'Building');
            if (firstBuilding) {
              this.selectBuild(firstBuilding); // isUserAction=false → auto-follow mode
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

        // Queued → Building: reload logs for the newly started build
        if (prevSelected?.status === 'Queued' && newSelected?.status === 'Building') {
          this.logLines = [];
          this.pendingLogLines = [];
          this.logHtml.set('');
          this.logLoading.set(true);
          this.fetchInitialLogs(newSelected.id);
        }

        // Building → Completed: flush logs.
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

    this.evalService.getBuilds(this.evaluationId, this.PAGE_SIZE, offset).subscribe({
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

    this.liveSub = this.live
      .connect(`/evals/${this.evaluationId}/live`)
      .pipe(
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
      });
  }

  stopLiveUpdates(): void {
    this.liveSub?.unsubscribe();
    this.liveSub = undefined;
  }

  setSidebarFocus(v: boolean): void { this.sidebarFocused = v; }

  // ── Build selection & log loading ──────────────────────────────────────────

  selectEvaluationSection(): void {
    this.selectedSection.set('messages');
    this.selectedBuildId.set(null);
    this.stopActiveStream();
    this.logLines = [];
    this.logHtml.set('');
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { build: null, eval: 1 },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  /// Resolves a deep-linked `?build=<id>`. If the build is already loaded we
  /// select it; otherwise we fetch that single build directly and inject it into
  /// the sidebar so its log shows immediately, rather than paging through the
  /// whole evaluation. `sortBuilds` dedups it once pagination catches up.
  private resolveInitialBuild(): void {
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
    this.evalService.getBuild(id).subscribe({
      next: (b: BuildWithOutputs) => {
        this.fetchingInitialBuild = false;
        if (this.initialBuildId !== id) return;
        const item: BuildItem = {
          id: b.id,
          name: b.derivation_path,
          status: b.status,
          has_artefacts: false,
          updated_at: b.updated_at,
          build_time_ms: null,
        };
        this.initialBuildId = null;
        this.builds.update(cur => this.sortBuilds([item, ...cur]));
        this.visibleBuilds.update(cur => this.sortBuilds([item, ...cur]));
        this.selectBuild(item, true);
      },
      error: () => { this.fetchingInitialBuild = false; },
    });
  }

  selectBuild(build: BuildItem, isUserAction = false): void {
    this.selectedSection.set(null);
    if (this.selectedBuildId() === build.id) return;

    this.userPickedBuild = isUserAction;
    this.autoFollowBuilding = false;
    this.stopActiveStream();
    this.logLines = [];
    this.logHtml.set('');
    this.logLineCount.set(0);
    this.selectedBuildId.set(build.id);
    this.autoScroll.set(true);
    this.showScrollBtn.set(false);

    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { build: build.id, eval: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });

    this.logLoading.set(true);
    this.fetchInitialLogs(build.id);
  }

  private async fetchInitialLogs(buildId: string): Promise<void> {
    this.resetChunkedState();
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
          // data.message is already a decoded JS string - split directly to preserve blank lines
          const lines = (data.message as string).split('\n');
          // Trim a single trailing empty string produced by a final newline
          if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();
          this.logLines = lines;
          this.renderLog();
        }
      }
    } catch {
      // ignore
    }

    this.logLoading.set(false);
    this.scrollToBottomIfAuto();

    // If this build is still running, start streaming
    const build = this.builds().find(b => b.id === buildId);
    const isBuilding = build && ['Queued', 'Building'].includes(build.status);
    if (isBuilding) {
      this.startLogStream(buildId);
    }
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
    }
  }

  private stopActiveStream(): void {
    try { this.activeStreamReader?.cancel(); } catch { /* ignore */ }
    this.activeStreamReader = undefined;
    this.streamingBuildId = undefined;
    this.pendingLogLines = [];
    this.stopLogDrainTimer();
  }

  // ── Chunked log viewing ─────────────────────────────────────────────────────

  private resetChunkedState(): void {
    this.chunkedMode.set(false);
    this.chunkIndex = null;
    this.logLineCount.set(0);
    this.windowText = [];
    this.windowStart = 1;
    this.windowLines.set([]);
    this.topSpacerPx.set(0);
    this.bottomSpacerPx.set(0);
    this.highlightLine.set(null);
    this.searchOpen.set(false);
    this.searchHits.set([]);
    this.searchTotal.set(0);
    this.currentHit.set(-1);
  }

  /** Returns true when the build has finalized chunks and chunked mode is active. */
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

    this.chunkIndex = index;
    this.chunkedMode.set(true);
    this.logLineCount.set(index.total_lines);

    const target = this.pendingDeepLink;
    this.pendingDeepLink = null;
    if (target && target <= index.total_lines) {
      await this.jumpToLine(buildId, target);
    } else {
      const start = Math.max(1, index.total_lines - this.WINDOW_PAGE + 1);
      await this.loadWindow(buildId, start, index.total_lines, 'replace');
      this.scrollToBottomIfAuto();
    }
    return true;
  }

  private async fetchLines(buildId: string, start: number, end: number): Promise<string[]> {
    const res = await fetch(
      `${environment.apiUrl}/builds/${buildId}/log/lines?start=${start}&end=${end}`,
      { method: 'GET', credentials: 'include' },
    );
    if (!res.ok) return [];
    const text = await res.text();
    const lines = text.split('\n');
    if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();
    return lines;
  }

  private async loadWindow(
    buildId: string,
    start: number,
    end: number,
    mode: 'replace' | 'append' | 'prepend',
  ): Promise<void> {
    if (this.loadingWindow) return;
    this.loadingWindow = true;
    try {
      const lines = await this.fetchLines(buildId, start, end);
      if (mode === 'replace') {
        this.windowStart = start;
        this.windowText = lines;
      } else if (mode === 'append') {
        this.windowText = this.windowText.concat(lines);
        if (this.windowText.length > this.MAX_WINDOW) {
          const drop = this.windowText.length - this.MAX_WINDOW;
          this.windowText = this.windowText.slice(drop);
          this.windowStart += drop;
        }
      } else {
        this.windowText = lines.concat(this.windowText);
        this.windowStart = start;
        if (this.windowText.length > this.MAX_WINDOW) {
          this.windowText = this.windowText.slice(0, this.MAX_WINDOW);
        }
      }
      this.renderWindow();
    } finally {
      this.loadingWindow = false;
    }
  }

  private renderWindow(): void {
    const total = this.chunkIndex?.total_lines ?? this.windowText.length;
    const rendered = this.windowText.map((text, i) => ({
      n: this.windowStart + i,
      html: this.sanitizer.bypassSecurityTrustHtml(this.convertAnsiToHtml(text)),
    }));
    this.windowLines.set(rendered);
    const before = this.windowStart - 1;
    const after = Math.max(0, total - (this.windowStart - 1 + this.windowText.length));
    this.topSpacerPx.set(before * this.LINE_PX);
    this.bottomSpacerPx.set(after * this.LINE_PX);
    this.cdr.detectChanges();
  }

  private async jumpToLine(buildId: string, line: number): Promise<void> {
    const total = this.chunkIndex?.total_lines ?? 0;
    const { start, end } = windowAround(total, line, this.WINDOW_PAGE);
    await this.loadWindow(buildId, start, end, 'replace');
    this.highlightLine.set(line);
    this.autoScroll.set(false);
    setTimeout(() => {
      const el = this.logContainerRef?.nativeElement;
      const target = el?.querySelector(`[data-line="${line}"]`) as HTMLElement | null;
      target?.scrollIntoView({ block: 'center' });
    }, 0);
  }

  selectLine(line: number): void {
    this.highlightLine.set(line);
    this.router.navigate([], {
      relativeTo: this.route,
      fragment: `L${line}`,
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
    const url = `${window.location.origin}${window.location.pathname}${window.location.search}#L${line}`;
    navigator.clipboard?.writeText(url).catch(() => { /* ignore */ });
  }

  // ── Search shortcuts ──────────────────────────────────────────────────────

  @HostListener('document:keydown', ['$event'])
  onKeydown(event: KeyboardEvent): void {
    const isFind = (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f';

    // Reveal the builds search on "/" (when not already typing) or on Ctrl/Cmd+F
    // while the sidebar holds focus.
    if ((event.key === '/' && !isTypingTarget(event.target)) || (isFind && this.sidebarFocused)) {
      event.preventDefault();
      this.openSidebarSearch();
      return;
    }

    if (event.key === 'Escape' && this.sidebarSearchOpen()) {
      this.closeSidebarSearch();
      return;
    }

    if (!this.chunkedMode() || !this.selectedBuildId()) return;
    if (isFind) {
      event.preventDefault();
      this.searchOpen.set(true);
      setTimeout(() => { (document.querySelector('.log-search-input') as HTMLInputElement | null)?.focus(); }, 0);
    } else if (event.key === 'Escape' && this.searchOpen()) {
      this.closeSearch();
    }
  }

  openSidebarSearch(): void {
    this.sidebarSearchOpen.set(true);
    setTimeout(() => { (document.querySelector('.sidebar-search input') as HTMLInputElement | null)?.focus(); }, 0);
  }

  closeSidebarSearch(): void {
    this.sidebarSearchOpen.set(false);
    this.sidebarSearchQuery.set('');
  }

  onSearchInput(event: Event): void {
    this.searchQuery.set((event.target as HTMLInputElement).value);
    this.scheduleSearch();
  }

  /// Debounced auto-search: only fires once typing pauses and at least
  /// MIN_SEARCH_LEN characters are present. Shorter queries reset results.
  private scheduleSearch(): void {
    if (this.searchDebounceTimer !== undefined) clearTimeout(this.searchDebounceTimer);
    const q = this.searchQuery().trim();
    if (q.length < this.MIN_SEARCH_LEN) {
      this.searchSeq++;
      this.searchLoading.set(false);
      this.searchHits.set([]);
      this.searchTotal.set(0);
      this.currentHit.set(-1);
      return;
    }
    this.searchLoading.set(true);
    this.searchDebounceTimer = setTimeout(() => {
      this.searchDebounceTimer = undefined;
      this.runSearch();
    }, this.SEARCH_DEBOUNCE_MS);
  }

  closeSearch(): void {
    this.searchOpen.set(false);
    this.highlightLine.set(null);
    if (this.searchDebounceTimer !== undefined) clearTimeout(this.searchDebounceTimer);
    this.searchSeq++;
    this.searchLoading.set(false);
  }

  async runSearch(): Promise<void> {
    const buildId = this.selectedBuildId();
    const q = this.searchQuery().trim();
    if (!buildId || q.length < this.MIN_SEARCH_LEN) {
      this.searchLoading.set(false);
      return;
    }
    const seq = ++this.searchSeq;
    this.searchHits.set([]);
    this.searchTotal.set(0);
    this.currentHit.set(-1);
    this.searchLoading.set(true);
    try {
      const res = await fetch(
        `${environment.apiUrl}/builds/${buildId}/log/search?q=${encodeURIComponent(q)}`,
        { method: 'GET', credentials: 'include' },
      );
      if (!res.ok || !res.body) return;
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffered = '';
      const hits: LogSearchHit[] = [];
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        // A newer search superseded this one - abandon the stale stream.
        if (seq !== this.searchSeq) {
          await reader.cancel().catch(() => { /* ignore */ });
          return;
        }
        buffered += decoder.decode(value, { stream: true });
        const parts = buffered.split('\n');
        buffered = parts.pop() ?? '';
        for (const part of parts) {
          if (!part.trim()) continue;
          const obj = JSON.parse(part);
          if (obj.done === true) {
            this.searchTotal.set(obj.total_matches ?? hits.length);
          } else {
            hits.push(obj as LogSearchHit);
            this.searchHits.set([...hits]);
          }
        }
      }
      if (hits.length > 0) this.gotoHit(0);
    } catch {
      // ignore
    } finally {
      if (seq === this.searchSeq) this.searchLoading.set(false);
    }
  }

  gotoHit(i: number): void {
    const hits = this.searchHits();
    if (hits.length === 0) return;
    const idx = ((i % hits.length) + hits.length) % hits.length;
    this.currentHit.set(idx);
    const buildId = this.selectedBuildId();
    if (buildId) this.jumpToLine(buildId, hits[idx].line_number);
  }

  nextHit(): void { this.gotoHit(this.currentHit() + 1); }
  prevHit(): void { this.gotoHit(this.currentHit() - 1); }

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

  private startLogDrainTimer(): void {
    if (this.logDrainTimer !== undefined) return;
    this.logDrainTimer = setInterval(() => {
      if (this.pendingLogLines.length === 0) return;
      // Drain faster when the queue is large so we never fall too far behind
      const count = this.pendingLogLines.length > 30
        ? Math.ceil(this.pendingLogLines.length / 8)
        : 1;
      this.logLines.push(...this.pendingLogLines.splice(0, count));
      this.renderLog();
      this.scrollToBottomIfAuto();
    }, 80);
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
      this.logLines.push(...this.pendingLogLines);
      this.pendingLogLines = [];
      this.renderLog();
      this.scrollToBottomIfAuto();
    }
  }

  // ── Log parsing & rendering ─────────────────────────────────────────────────

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

  private readonly ansiColorMap: Record<string, string> = {
    // Styles
    '1': '<span style="font-weight:bold">',
    '2': '<span style="opacity:0.6">',
    '3': '<span style="font-style:italic">',
    // Standard foreground (30-37)
    '30': '<span style="color:#374151">',
    '31': '<span style="color:#ef4444">',
    '32': '<span style="color:#22c55e">',
    '33': '<span style="color:#eab308">',
    '34': '<span style="color:#3b82f6">',
    '35': '<span style="color:#a855f7">',
    '36': '<span style="color:#06b6d4">',
    '37': '<span style="color:#d1d5db">',
    // Bright foreground (90-97)
    '90': '<span style="color:#6b7280">',
    '91': '<span style="color:#f87171">',
    '92': '<span style="color:#4ade80">',
    '93': '<span style="color:#fbbf24">',
    '94': '<span style="color:#60a5fa">',
    '95': '<span style="color:#c084fc">',
    '96': '<span style="color:#22d3ee">',
    '97': '<span style="color:#f9fafb">',
    // Combined bold+color (common nix patterns)
    '1;31': '<span style="color:#ef4444;font-weight:bold">',
    '1;32': '<span style="color:#22c55e;font-weight:bold">',
    '1;33': '<span style="color:#eab308;font-weight:bold">',
    '1;34': '<span style="color:#3b82f6;font-weight:bold">',
    '1;35': '<span style="color:#a855f7;font-weight:bold">',
    '1;36': '<span style="color:#06b6d4;font-weight:bold">',
    '31;1': '<span style="color:#ef4444;font-weight:bold">',
    '35;1': '<span style="color:#a855f7;font-weight:bold">',
  };

  private static readonly ANSI_RESETS = new Set(['0', '', '39', '49', '22', '23', '24']);

  private convertAnsiToHtml(text: string): string {
    const escaped = text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
    let openSpans = 0;
    // Match any CSI sequence: ESC [ <param bytes> <final byte>
    // Only SGR sequences (final byte 'm') are converted to spans; all others are stripped.
    const result = escaped.replace(/\u001b\[([?0-9;]*)([A-Za-z~])/g, (_, params: string, cmd: string) => {
      if (cmd !== 'm') return '';
      if (EvaluationLogComponent.ANSI_RESETS.has(params)) {
        const closing = '</span>'.repeat(openSpans);
        openSpans = 0;
        return closing;
      }
      const tag = this.ansiColorMap[params];
      if (tag) { openSpans++; return tag; }
      return '';
    });
    return result + '</span>'.repeat(openSpans);
  }

  private renderLog(): void {
    const html = this.logLines.map(l => this.convertAnsiToHtml(l)).join('\n');
    this.logHtml.set(this.sanitizer.bypassSecurityTrustHtml(html));
    this.logLineCount.set(this.logLines.length);
    this.cdr.detectChanges();
  }

  // ── Scroll management ───────────────────────────────────────────────────────

  onBuildsViewportScroll(event: Event): void {
    const el = event.target as HTMLElement;
    // Pre-fetch the next page as the list nears the bottom.
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 240) {
      this.loadMoreBuilds();
    }
  }

  onLogScroll(event: Event): void {
    const el = event.target as HTMLElement;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    this.autoScroll.set(atBottom);
    this.showScrollBtn.set(!atBottom);
    if (this.chunkedMode()) this.maybePageChunked(el);
  }

  private maybePageChunked(el: HTMLElement): void {
    if (this.loadingWindow || !this.chunkIndex) return;
    const buildId = this.selectedBuildId();
    if (!buildId) return;
    const total = this.chunkIndex.total_lines;
    const windowEnd = this.windowStart + this.windowText.length - 1;

    // Fast scrollbar drag: the viewport jumped into a spacer region with no
    // rendered lines. Burst-load a fresh window centred on the new position
    // instead of crawling one page at a time from an edge.
    if (this.windowText.length > 0) {
      const loadedTopPx = this.topSpacerPx();
      const loadedBottomPx = loadedTopPx + this.windowText.length * this.LINE_PX;
      if (el.scrollTop + el.clientHeight < loadedTopPx || el.scrollTop > loadedBottomPx) {
        const centerLine = Math.min(
          total,
          Math.max(1, Math.round((el.scrollTop + el.clientHeight / 2) / this.LINE_PX)),
        );
        const { start, end } = windowAround(total, centerLine, this.WINDOW_PAGE);
        this.loadWindow(buildId, start, end, 'replace');
        return;
      }
    }

    if (el.scrollTop < 200 && this.windowStart > 1) {
      const start = Math.max(1, this.windowStart - this.WINDOW_PAGE);
      const end = this.windowStart - 1;
      const prevHeight = el.scrollHeight;
      const prevTop = el.scrollTop;
      this.loadWindow(buildId, start, end, 'prepend').then(() => {
        setTimeout(() => {
          el.scrollTop = prevTop + (el.scrollHeight - prevHeight);
        }, 0);
      });
    } else if (el.scrollHeight - el.scrollTop - el.clientHeight < 200 && windowEnd < total) {
      const start = windowEnd + 1;
      const end = Math.min(total, windowEnd + this.WINDOW_PAGE);
      this.loadWindow(buildId, start, end, 'append');
    }
  }

  scrollToBottom(): void {
    const el = this.logContainerRef?.nativeElement;
    if (el) {
      el.scrollTop = el.scrollHeight;
      this.autoScroll.set(true);
      this.showScrollBtn.set(false);
    }
  }

  private scrollToBottomIfAuto(): void {
    if (this.autoScroll()) {
      setTimeout(() => this.scrollToBottom(), 0);
    }
  }

  // ── Duration ─────────────────────────────────────────────────────────────────

  private startDurationTimer(evaluation: Evaluation): void {
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

  private updateDuration(evaluation: Evaluation): void {
    const start = parseUtcTimestamp(evaluation.created_at);
    const end = this.isRunningStatus(evaluation.status)
      ? Date.now()
      : parseUtcTimestamp(evaluation.updated_at);
    this.duration.set(formatEvaluationDuration(end - start));
  }

  isRunningStatus(status: EvaluationStatus): boolean {
    return isRunningEvaluationStatus(status);
  }

  // ── Abort ───────────────────────────────────────────────────────────────────

  abortEvaluation(): void {
    this.aborting.set(true);
    this.evalService.abortEvaluation(this.evaluationId).subscribe({
      next: () => {
        this.aborting.set(false);
        this.loadEvaluation();
      },
      error: () => this.aborting.set(false),
    });
  }

  // ── Helpers ──────────────────────────────────────────────────────────────────

  selectAdjacentBuild(index: number): void {
    const list = this.visibleBuilds();
    if (index < 0 || index >= list.length) return;
    this.selectBuild(list[index], true);
    const targetId = list[index].id;
    setTimeout(() => {
      const el = document.querySelector<HTMLElement>(`.build-item[data-build-id="${targetId}"]`);
      el?.scrollIntoView({ block: 'nearest' });
      el?.focus();
    }, 0);
  }

  buildDisplayName(path: string): string {
    // /nix/store/hash-name-version.drv → name-version (strip hash prefix only)
    const filename = path.split('/').pop() ?? path;
    return filename.replace(/^[^-]+-/, '').replace(/\.drv$/, '');
  }

  getBuildElapsed(build: BuildItem): string {
    if (build.status === 'Completed') {
      if (build.build_time_ms !== null) return this.formatMs(build.build_time_ms);
      return '';
    }
    if (build.status === 'Building') {
      this.tick(); // reactive dependency - re-evaluated every second
      const ts = build.updated_at;
      const start = new Date(ts.includes('Z') || ts.includes('+') ? ts : ts + 'Z');
      return this.formatMs(Math.max(0, Date.now() - start.getTime()));
    }
    return '';
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

  formatWaitingReason(reason: WaitingReason): string {
    switch (reason.kind) {
      case 'workers': {
        if (reason.connected_workers === 0) {
          return 'No workers are connected. The evaluation requires:';
        }
        const archList = reason.available_architectures.length > 0
          ? reason.available_architectures.join(', ')
          : 'none';
        const workerWord = reason.connected_workers === 1 ? 'worker' : 'workers';
        return `${reason.connected_workers} connected ${workerWord} (${archList}) cannot satisfy:`;
      }
      case 'approval':
        return `Awaiting maintainer approval for pull request #${reason.pr_number} by ${reason.pr_author}.`;
      case 'no_cache':
        return 'No cache is configured for this organization. Configure a cache before this evaluation can run.';
    }
  }

  waitingTitle(reason: WaitingReason | undefined): string {
    switch (reason?.kind) {
      case 'approval': return 'Awaiting Approval';
      case 'no_cache': return 'No Cache Configured';
      default: return 'Waiting for Workers';
    }
  }

  isWorkersWaiting(reason: WaitingReason | undefined): boolean {
    return reason?.kind === 'workers';
  }

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

  navigateToEvaluation(id: string): void {
    this.stopLiveUpdates();
    this.stopDurationTimer();
    this.stopActiveStream();
    this.selectedBuildId.set(null);
    this.selectedSection.set(null);
    this.messages.set([]);
    this.logLines = [];
    this.logHtml.set('');
    this.isInitialBuildsLoad = true;
    this.totalBuilds = 0;
    this.loadingMore = false;
    this.evaluationId = id;
    this.router.navigate(['/organization', this.orgName, 'log', id]);
    this.loadEvaluation();
  }
}
