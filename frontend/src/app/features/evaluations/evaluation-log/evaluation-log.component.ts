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
} from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';
import { interval, Subscription } from 'rxjs';
import { switchMap } from 'rxjs/operators';
import { CdkVirtualScrollViewport, ScrollingModule } from '@angular/cdk/scrolling';
import { EvaluationsService, BuildItem } from '@core/services/evaluations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { Evaluation, EvaluationMessage } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ButtonModule } from 'primeng/button';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-evaluation-log',
  standalone: true,
  imports: [CommonModule, RouterModule, LoadingSpinnerComponent, ButtonModule, ScrollingModule],
  templateUrl: './evaluation-log.component.html',
  styleUrl: './evaluation-log.component.scss',
})
export class EvaluationLogComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private orgsService = inject(OrganizationsService);
  protected authService = inject(AuthService);
  private sanitizer = inject(DomSanitizer);
  private cdr = inject(ChangeDetectorRef);

  @ViewChild('logContainer') logContainerRef?: ElementRef<HTMLDivElement>;
  @ViewChild('buildsViewport') buildsViewport?: CdkVirtualScrollViewport;

  loading = signal(true);
  evaluation = signal<Evaluation | null>(null);
  builds = signal<BuildItem[]>([]);
  messages = signal<(EvaluationMessage & { renderedHtml: SafeHtml })[]>([]);
  selectedBuildId = signal<string | null>(null);
  selectedSection = signal<'messages' | null>(null);
  logHtml = signal<SafeHtml>('');
  logLoading = signal(true);
  aborting = signal(false);
  autoScroll = signal(true);
  showScrollBtn = signal(false);
  duration = signal('0:00');
  totalBuildsCount = signal(0);
  activeBuildsCount = signal(0);
  private tick = signal(0);

  orgName = '';
  orgDisplayName = signal('');
  evaluationId = '';
  private initialBuildId: string | null = null;
  private initialShowEval = false;

  // Derived from backend totals — accurate regardless of how many builds are loaded.
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

  private pollSub?: Subscription;
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

  ngOnInit(): void {
    document.documentElement.style.overflow = 'hidden';
    document.body.style.overflow = 'hidden';
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.evaluationId = this.route.snapshot.paramMap.get('evaluationId') || '';
    this.initialBuildId = this.route.snapshot.queryParamMap.get('build');
    this.initialShowEval = this.route.snapshot.queryParamMap.get('eval') !== null;
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
    this.stopPolling();
    this.stopDurationTimer();
    this.stopActiveStream();
    this.stopBuildRevealTimer();
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
        this.startPollingIfRunning(evaluation.status);
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
    Building: 0,
    Queued: 1,
    Failed: 2,
    Aborted: 3,
    DependencyFailed: 3,
    Completed: 4,
    Substituted: 4,
  };

  private sortBuilds(builds: BuildItem[]): BuildItem[] {
    return [...builds].sort((a, b) => {
      const oa = this.buildStatusOrder[a.status] ?? 99;
      const ob = this.buildStatusOrder[b.status] ?? 99;
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

        if (this.initialBuildId) {
          const target = this.builds().find(b => b.id === this.initialBuildId);
          if (target) {
            this.initialBuildId = null;
            this.selectBuild(target, true);
          }
        }

        // Auto-fetch more pages until all active builds are in memory.
        // This ensures status transitions and log streaming work for every build.
        if (this.builds().length < result.active_count && !this.loadingMore) {
          this.doLoadMore();
        }
      },
    });
  }

  /** Triggered by scroll proximity — pre-fetches next page before user hits bottom. */
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
        // Do NOT update totalBuildsCount / activeBuildsCount here — those metric signals
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

        // Resolve pending initialBuildId that may have been beyond the first page
        if (this.initialBuildId) {
          const target = this.builds().find(b => b.id === this.initialBuildId);
          if (target) {
            this.initialBuildId = null;
            this.selectBuild(target, true);
          }
        }

        // Continue fetching if there are still active builds not yet in memory
        const stillNeedsMore = this.builds().length < result.active_count
          || (this.initialBuildId !== null && this.builds().length < this.totalBuilds);
        if (stillNeedsMore) {
          this.doLoadMore();
        }
      },
      error: () => {
        this.loadingMore = false;
      },
    });
  }

  startPollingIfRunning(status: string): void {
    this.stopPolling();
    const running = ['Queued', 'Fetching', 'EvaluatingFlake', 'EvaluatingDerivation', 'Building', 'Waiting'];
    if (!running.includes(status)) return;

    this.pollSub = interval(5000)
      .pipe(switchMap(() => this.evalService.getEvaluation(this.evaluationId)))
      .subscribe({
        next: (evaluation) => {
          this.evaluation.set(evaluation);
          this.loadBuilds();
          if (!running.includes(evaluation.status)) {
            this.stopPolling();
            this.stopDurationTimer();
            this.loadBuilds(); // final update
            this.loadMessages(); // pick up any messages recorded during eval
          }
        },
      });
  }

  stopPolling(): void {
    this.pollSub?.unsubscribe();
    this.pollSub = undefined;
  }

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

  selectBuild(build: BuildItem, isUserAction = false): void {
    this.selectedSection.set(null);
    if (this.selectedBuildId() === build.id) return;

    this.userPickedBuild = isUserAction;
    this.autoFollowBuilding = false;
    this.stopActiveStream();
    this.logLines = [];
    this.logHtml.set('');
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
    try {
      const response = await fetch(`${environment.apiUrl}/builds/${buildId}/log`, {
        method: 'GET',
        credentials: 'include',
      });

      if (response.ok) {
        const data = await response.json();
        if (!data.error && typeof data.message === 'string' && data.message !== '') {
          // data.message is already a decoded JS string — split directly to preserve blank lines
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

  // ── Build reveal animation ──────────────────────────────────────────────────

  private startBuildRevealTimer(): void {
    if (this.buildRevealTimer !== undefined) return;
    this.buildRevealTimer = setInterval(() => {
      if (this.pendingBuilds.length === 0) {
        this.stopBuildRevealTimer();
        return;
      }
      const next = this.pendingBuilds.shift()!;
      this.visibleBuilds.update(vbs => [next, ...vbs]);
    }, 50);
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
    this.cdr.detectChanges();
  }

  // ── Scroll management ───────────────────────────────────────────────────────

  onBuildsViewportScroll(): void {
    const vp = this.buildsViewport;
    if (!vp) return;
    const total = vp.getDataLength();
    const end = vp.getRenderedRange().end;
    // Pre-fetch next page when rendered range reaches within ~20 rows of the end
    if (total > 0 && end >= total - 20) {
      this.loadMoreBuilds();
    }
  }

  onLogScroll(event: Event): void {
    const el = event.target as HTMLElement;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    this.autoScroll.set(atBottom);
    this.showScrollBtn.set(!atBottom);
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

    const running = ['Queued', 'Fetching', 'EvaluatingFlake', 'EvaluatingDerivation', 'Building', 'Waiting'];
    if (!running.includes(evaluation.status)) return;

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
    const ts = evaluation.created_at;
    const start = new Date(
      ts.includes('Z') || ts.includes('+') ? ts : ts + 'Z'
    );
    const ms = Math.max(0, new Date().getTime() - start.getTime());
    const totalSecs = Math.floor(ms / 1000);
    const h = Math.floor(totalSecs / 3600);
    const m = Math.floor((totalSecs % 3600) / 60);
    const s = totalSecs % 60;

    this.duration.set(
      h > 0
        ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
        : `${m}:${String(s).padStart(2, '0')}`
    );
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
    // Ensure the target row is rendered (virtual scroll may have recycled it)
    this.buildsViewport?.scrollToIndex(index);
    const targetId = list[index].id;
    setTimeout(() => {
      const el = document.querySelector<HTMLElement>(`.build-item[data-build-id="${targetId}"]`);
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
      this.tick(); // reactive dependency — re-evaluated every second
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

  getStatusLabel(status: string): string {
    if (status === 'Fetching') return 'Fetching';
    if (status === 'EvaluatingFlake' || status === 'EvaluatingDerivation') return 'Evaluating';
    return status;
  }

  navigateToEvaluation(id: string): void {
    this.stopPolling();
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
