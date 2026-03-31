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
import { EvaluationsService, BuildItem } from '@core/services/evaluations.service';
import { Evaluation } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ButtonModule } from 'primeng/button';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-evaluation-log',
  standalone: true,
  imports: [CommonModule, RouterModule, LoadingSpinnerComponent, ButtonModule],
  templateUrl: './evaluation-log.component.html',
  styleUrl: './evaluation-log.component.scss',
})
export class EvaluationLogComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  protected authService = inject(AuthService);
  private sanitizer = inject(DomSanitizer);
  private cdr = inject(ChangeDetectorRef);

  @ViewChild('logContainer') logContainerRef?: ElementRef<HTMLDivElement>;

  loading = signal(true);
  evaluation = signal<Evaluation | null>(null);
  builds = signal<BuildItem[]>([]);
  selectedBuildId = signal<string | null>(null);
  logHtml = signal<SafeHtml>('');
  logLoading = signal(false);
  aborting = signal(false);
  autoScroll = signal(true);
  showScrollBtn = signal(false);
  duration = signal('0:00');

  orgName = '';
  evaluationId = '';
  private initialBuildId: string | null = null;

  completedBuilds = computed(() =>
    this.builds().filter(b => b.status === 'Completed').length
  );

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

  private pollSub?: Subscription;
  private durationInterval?: ReturnType<typeof setInterval>;
  private activeStreamReader?: ReadableStreamDefaultReader<Uint8Array>;
  private streamingBuildId?: string;
  private logLines: string[] = [];
  private autoFollowBuilding = false;
  private isInitialBuildsLoad = true;
  private pendingBuilds: BuildItem[] = [];
  private buildRevealTimer?: ReturnType<typeof setInterval>;
  private pendingLogLines: string[] = [];
  private logDrainTimer?: ReturnType<typeof setInterval>;

  ngOnInit(): void {
    document.body.style.overflow = 'hidden';
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.evaluationId = this.route.snapshot.paramMap.get('evaluationId') || '';
    this.initialBuildId = this.route.snapshot.queryParamMap.get('build');
    if (!this.evaluationId) {
      this.loading.set(false);
      return;
    }
    this.loadEvaluation();
  }

  ngOnDestroy(): void {
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
        this.startDurationTimer(evaluation);
        this.startPollingIfRunning(evaluation.status);
      },
      error: () => this.loading.set(false),
    });
  }

  private readonly buildStatusOrder: Record<string, number> = {
    Building: 0,
    Queued: 1,
    Failed: 2,
    Aborted: 3,
    Completed: 4,
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
    this.evalService.getBuilds(this.evaluationId).subscribe({
      next: (builds) => {
        const prevSelected = this.selectedBuild();
        this.builds.set(this.sortBuilds(builds));
        const newSelected = this.selectedBuild();
        const isEvaluating = this.evaluation()?.status === 'Evaluating';

        // ── Build list visibility ───────────────────────────────────────────
        if (this.isInitialBuildsLoad) {
          // First load: always show everything immediately
          this.visibleBuilds.set(this.builds());
          this.isInitialBuildsLoad = false;
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

        // Building → Completed: flush all pending log lines, then follow next build
        if (prevSelected?.status === 'Building' && newSelected?.status === 'Completed') {
          this.flushPendingLogs();
          const next = this.builds().find(b => b.status === 'Building');
          if (next) {
            this.selectBuild(next);
          } else {
            this.autoFollowBuilding = true;
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
          const target = builds.find(b => b.id === this.initialBuildId);
          if (target) {
            this.initialBuildId = null;
            this.selectBuild(target);
          }
        }
      },
    });
  }

  startPollingIfRunning(status: string): void {
    this.stopPolling();
    const running = ['Queued', 'Evaluating', 'Building'];
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
          }
        },
      });
  }

  stopPolling(): void {
    this.pollSub?.unsubscribe();
    this.pollSub = undefined;
  }

  // ── Build selection & log loading ──────────────────────────────────────────

  selectBuild(build: BuildItem): void {
    if (this.selectedBuildId() === build.id) return;

    this.autoFollowBuilding = false;
    this.stopActiveStream();
    this.logLines = [];
    this.logHtml.set('');
    this.selectedBuildId.set(build.id);
    this.autoScroll.set(true);
    this.showScrollBtn.set(false);

    this.logLoading.set(true);
    this.fetchInitialLogs(build.id);
  }

  private async fetchInitialLogs(buildId: string): Promise<void> {
    try {
      const token = localStorage.getItem('jwt_token') || sessionStorage.getItem('jwt_token') || '';
      const headers: Record<string, string> = token ? { Authorization: `Bearer ${token}` } : {};
      const response = await fetch(`${environment.apiUrl}/builds/${buildId}/log`, {
        method: 'GET',
        headers,
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
      const token = localStorage.getItem('jwt_token') || sessionStorage.getItem('jwt_token') || '';
      if (!token) return;
      const response = await fetch(`${environment.apiUrl}/builds/${buildId}/log`, {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/jsonstream',
        },
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
    }, 200);
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
    // Reset / close
    '0':    '</span>', '39': '</span>', '49': '</span>',
    '22': '</span>', '23': '</span>', '24': '</span>',
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

  private convertAnsiToHtml(text: string): string {
    const escaped = text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
    return escaped.replace(/\u001b\[([0-9;]*)m/g, (_, code: string) => {
      return this.ansiColorMap[code] ?? '';
    });
  }

  private renderLog(): void {
    const html = this.logLines.map(l => this.convertAnsiToHtml(l)).join('\n');
    this.logHtml.set(this.sanitizer.bypassSecurityTrustHtml(html));
    this.cdr.detectChanges();
  }

  // ── Scroll management ───────────────────────────────────────────────────────

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

    const running = ['Queued', 'Evaluating', 'Building'];
    if (!running.includes(evaluation.status)) return;

    this.durationInterval = setInterval(() => {
      const ev = this.evaluation();
      if (ev) this.updateDuration(ev);
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
    this.selectBuild(list[index]);
    setTimeout(() => {
      const items = document.querySelectorAll<HTMLElement>('.build-item');
      items[index]?.focus();
    }, 0);
  }

  buildDisplayName(path: string): string {
    // /nix/store/hash-name-version.drv → name-version (strip hash prefix only)
    const filename = path.split('/').pop() ?? path;
    return filename.replace(/^[^-]+-/, '').replace(/\.drv$/, '');
  }

  isRunning(): boolean {
    const s = this.evaluation()?.status;
    return s === 'Queued' || s === 'Evaluating' || s === 'Building';
  }

  navigateToEvaluation(id: string): void {
    this.stopPolling();
    this.stopDurationTimer();
    this.stopActiveStream();
    this.selectedBuildId.set(null);
    this.logLines = [];
    this.logHtml.set('');
    this.evaluationId = id;
    this.router.navigate(['/organization', this.orgName, 'log', id]);
    this.loadEvaluation();
  }
}
