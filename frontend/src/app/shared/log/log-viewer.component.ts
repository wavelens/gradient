/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  ChangeDetectorRef,
  Component,
  ElementRef,
  HostListener,
  OnDestroy,
  ViewChild,
  effect,
  inject,
  input,
  signal,
  untracked,
} from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';
import type { LogSearchHit } from '@core/models';
import { IconComponent, InputDirective } from '@gradient/ui/ui';
import { ansiToHtml } from './ansi';
import { searchLines, windowAround } from './log-window';

export interface ServerLog {
  total: number;
  lines(start: number, end: number): Promise<string[]>;
  search(query: string, onHits: (hits: LogSearchHit[]) => void, isStale: () => boolean): Promise<number>;
}

export type LogMarker = (line: string) => string | null;

interface RenderedLine {
  n: number;
  html: SafeHtml | null;
  marker: string | null;
}

type WindowMode = 'replace' | 'append' | 'prepend';

export function isFindShortcut(event: KeyboardEvent): boolean {
  return (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f';
}

function extendsLog(shown: readonly string[], next: readonly string[]): boolean {
  const kept = shown.length;
  return next.length >= kept && next[0] === shown[0] && (kept < 2 || next[kept - 2] === shown[kept - 2]);
}

@Component({
  selector: 'gr-log-viewer',
  standalone: true,
  imports: [IconComponent, InputDirective],
  templateUrl: './log-viewer.component.html',
  styleUrl: './log-viewer.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LogViewerComponent implements OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private sanitizer = inject(DomSanitizer);
  private cdr = inject(ChangeDetectorRef);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);

  lines = input<readonly string[]>([]);
  serverLog = input<ServerLog | null>(null);
  marker = input<LogMarker>(() => null);
  initialLine = input<number | null>(null);
  shortcuts = input(true);

  @ViewChild('logContainer') logContainerRef?: ElementRef<HTMLDivElement>;

  lineCount = signal(0);
  windowLines = signal<RenderedLine[]>([]);
  topSpacerPx = signal(0);
  bottomSpacerPx = signal(0);
  highlightLine = signal<number | null>(null);
  autoScroll = signal(true);
  showScrollBtn = signal(false);
  searchOpen = signal(false);
  searchQuery = signal('');
  searchHits = signal<LogSearchHit[]>([]);
  searchTotal = signal(0);
  currentHit = signal(-1);
  searchLoading = signal(false);

  private memory: readonly string[] = [];
  private server: ServerLog | null = null;
  private initialLineUsed = false;
  private windowStart = 1;
  private loadingWindow = false;
  private followScroll = false;
  private searchDebounceTimer?: ReturnType<typeof setTimeout>;
  private searchSeq = 0;
  private readonly MIN_SEARCH_LEN = 3;
  private readonly SEARCH_DEBOUNCE_MS = 300;
  private readonly LINE_PX = 18;
  private readonly WINDOW_PAGE = 800;
  private readonly MAX_WINDOW = 4000;

  constructor() {
    effect(() => {
      const server = this.serverLog();
      const lines = this.lines();
      untracked(() => (server ? this.showServerLog(server) : this.showLines(lines)));
    });
  }

  ngOnDestroy(): void {
    if (this.searchDebounceTimer !== undefined) clearTimeout(this.searchDebounceTimer);
  }

  private showServerLog(server: ServerLog): void {
    if (server === this.server) return;
    this.reset();
    this.server = server;
    this.lineCount.set(server.total);
    void this.showTailOrInitialLine();
  }

  private showLines(next: readonly string[]): void {
    const shown = this.memory;
    if (this.server || (shown.length > 0 && !extendsLog(shown, next))) this.reset();
    const kept = this.memory.length;
    this.memory = next;
    if (kept === 0) {
      this.lineCount.set(next.length);
      if (next.length > 0) void this.showTailOrInitialLine();
      return;
    }
    if (next[kept - 1] !== shown[kept - 1]) this.rerenderLine(kept);
    if (next.length > kept) this.appendStreamedLines(kept);
  }

  private reset(): void {
    this.server = null;
    this.memory = [];
    this.lineCount.set(0);
    this.windowStart = 1;
    this.windowLines.set([]);
    this.topSpacerPx.set(0);
    this.bottomSpacerPx.set(0);
    this.highlightLine.set(null);
    this.autoScroll.set(true);
    this.showScrollBtn.set(false);
    this.closeSearch();
    this.searchHits.set([]);
    this.searchTotal.set(0);
    this.currentHit.set(-1);
  }

  private async showTailOrInitialLine(): Promise<void> {
    const total = this.totalLines();
    const target = this.initialLineUsed ? null : this.initialLine();
    this.initialLineUsed = true;
    if (target && target <= total) {
      await this.jumpToLine(target);
    } else {
      await this.loadWindow(Math.max(1, total - this.WINDOW_PAGE + 1), total, 'replace');
      this.scrollToBottomIfAuto();
    }
  }

  private totalLines(): number {
    return this.server ? this.server.total : this.memory.length;
  }

  private async getLines(start: number, end: number): Promise<string[]> {
    return this.server ? this.server.lines(start, end) : this.memory.slice(start - 1, end);
  }

  private renderLine(text: string, n: number): RenderedLine {
    const marker = this.marker()(text);
    return { n, marker, html: marker === null ? this.sanitizer.bypassSecurityTrustHtml(ansiToHtml(text)) : null };
  }

  private renderLines(lines: readonly string[], startLine: number): RenderedLine[] {
    return lines.map((text, i) => this.renderLine(text, startLine + i));
  }

  private async loadWindow(start: number, end: number, mode: WindowMode): Promise<void> {
    if (this.loadingWindow) return;
    this.loadingWindow = true;
    try {
      const rendered = this.renderLines(await this.getLines(start, end), start);
      if (mode === 'replace') {
        this.windowStart = start;
        this.windowLines.set(rendered);
      } else if (mode === 'append') {
        this.setWindowTrimmingTop([...this.windowLines(), ...rendered]);
      } else {
        this.windowStart = start;
        this.windowLines.set([...rendered, ...this.windowLines()].slice(0, this.MAX_WINDOW));
      }
      this.updateSpacers();
    } finally {
      this.loadingWindow = false;
    }
  }

  private setWindowTrimmingTop(next: RenderedLine[]): void {
    const drop = next.length - this.MAX_WINDOW;
    if (drop > 0) {
      next = next.slice(drop);
      this.windowStart += drop;
    }
    this.windowLines.set(next);
  }

  private updateSpacers(): void {
    const after = Math.max(0, this.totalLines() - (this.windowStart - 1 + this.windowLines().length));
    this.topSpacerPx.set((this.windowStart - 1) * this.LINE_PX);
    this.bottomSpacerPx.set(after * this.LINE_PX);
    this.cdr.detectChanges();
  }

  private appendStreamedLines(firstNew: number): void {
    const total = this.memory.length;
    const atTail = this.windowStart + this.windowLines().length - 1 === firstNew;
    this.lineCount.set(total);

    if (this.autoScroll()) {
      if (atTail) {
        this.setWindowTrimmingTop([...this.windowLines(), ...this.renderLines(this.memory.slice(firstNew), firstNew + 1)]);
      } else {
        void this.loadWindow(Math.max(1, total - this.WINDOW_PAGE + 1), total, 'replace');
      }
    }

    this.updateSpacers();
    this.scrollToBottomIfAuto(true);
  }

  private rerenderLine(n: number): void {
    const index = n - this.windowStart;
    const window = this.windowLines();
    if (index < 0 || index >= window.length) return;
    const next = [...window];
    next[index] = this.renderLine(this.memory[n - 1], n);
    this.windowLines.set(next);
  }

  async jumpToLine(line: number): Promise<void> {
    const { start, end } = windowAround(this.totalLines(), line, this.WINDOW_PAGE);
    await this.loadWindow(start, end, 'replace');
    this.highlightLine.set(line);
    this.autoScroll.set(false);
    setTimeout(() => {
      const target = this.logContainerRef?.nativeElement.querySelector<HTMLElement>(`[data-line="${line}"]`);
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
    navigator.clipboard?.writeText(url).catch(() => undefined);
  }

  @HostListener('document:keydown', ['$event'])
  onDocumentKeydown(event: KeyboardEvent): void {
    if (!this.shortcuts()) return;
    if (isFindShortcut(event)) {
      event.preventDefault();
      this.openSearch();
    } else if (event.key === 'Escape' && this.searchOpen()) {
      this.closeSearch();
    }
  }

  openSearch(): void {
    this.searchOpen.set(true);
    setTimeout(() => this.host.nativeElement.querySelector<HTMLInputElement>('.log-search-input')?.focus(), 0);
  }

  closeSearch(): void {
    this.searchOpen.set(false);
    this.highlightLine.set(null);
    if (this.searchDebounceTimer !== undefined) clearTimeout(this.searchDebounceTimer);
    this.searchSeq++;
    this.searchLoading.set(false);
  }

  onSearchInput(event: Event): void {
    this.searchQuery.set((event.target as HTMLInputElement).value);
    this.scheduleSearch();
  }

  private scheduleSearch(): void {
    if (this.searchDebounceTimer !== undefined) clearTimeout(this.searchDebounceTimer);
    if (this.searchQuery().trim().length < this.MIN_SEARCH_LEN) {
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
      void this.runSearch();
    }, this.SEARCH_DEBOUNCE_MS);
  }

  async runSearch(): Promise<void> {
    const query = this.searchQuery().trim();
    if (query.length < this.MIN_SEARCH_LEN) {
      this.searchLoading.set(false);
      return;
    }
    const seq = ++this.searchSeq;
    const server = this.server;
    if (!server) {
      const hits = searchLines(this.memory, query);
      this.searchHits.set(hits);
      this.searchTotal.set(hits.length);
      this.searchLoading.set(false);
      if (hits.length > 0) this.gotoHit(0);
      return;
    }
    this.searchHits.set([]);
    this.searchTotal.set(0);
    this.currentHit.set(-1);
    this.searchLoading.set(true);
    const isStale = () => seq !== this.searchSeq;
    try {
      const total = await server.search(query, (hits) => this.searchHits.set(hits), isStale);
      if (isStale()) return;
      this.searchTotal.set(total);
      if (this.searchHits().length > 0) this.gotoHit(0);
    } catch {
      return;
    } finally {
      if (!isStale()) this.searchLoading.set(false);
    }
  }

  gotoHit(i: number): void {
    const hits = this.searchHits();
    if (hits.length === 0) return;
    const index = ((i % hits.length) + hits.length) % hits.length;
    this.currentHit.set(index);
    void this.jumpToLine(hits[index].line_number);
  }

  nextHit(): void { this.gotoHit(this.currentHit() + 1); }
  prevHit(): void { this.gotoHit(this.currentHit() - 1); }

  onLogScroll(event: Event): void {
    const el = event.target as HTMLElement;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (atBottom) {
      this.followScroll = false;
      this.autoScroll.set(true);
      this.showScrollBtn.set(false);
    } else if (!this.followScroll) {
      this.autoScroll.set(false);
      this.showScrollBtn.set(true);
    }
    this.maybePageLog(el);
  }

  // A smooth follow-scroll passes through positions above the bottom; only the
  // reader's own input may read those as leaving the tail.
  onLogWheel(event: WheelEvent): void {
    if (event.deltaY < 0) this.stopFollowScroll();
  }

  stopFollowScroll(): void {
    this.followScroll = false;
  }

  private maybePageLog(el: HTMLElement): void {
    if (this.loadingWindow) return;
    const total = this.totalLines();
    if (total === 0) return;
    const windowCount = this.windowLines().length;
    const windowEnd = this.windowStart + windowCount - 1;

    if (windowCount > 0) {
      const loadedTopPx = this.topSpacerPx();
      const loadedBottomPx = loadedTopPx + windowCount * this.LINE_PX;
      const outsideAbove = el.scrollTop + el.clientHeight < loadedTopPx && this.windowStart > 1;
      const outsideBelow = el.scrollTop > loadedBottomPx && windowEnd < total;
      if (outsideAbove || outsideBelow) {
        const centerLine = Math.min(total, Math.max(1, Math.round((el.scrollTop + el.clientHeight / 2) / this.LINE_PX)));
        const { start, end } = windowAround(total, centerLine, this.WINDOW_PAGE);
        void this.loadWindow(start, end, 'replace');
        return;
      }
    }

    if (el.scrollTop < 200 && this.windowStart > 1) {
      const start = Math.max(1, this.windowStart - this.WINDOW_PAGE);
      const end = this.windowStart - 1;
      const prevHeight = el.scrollHeight;
      const prevTop = el.scrollTop;
      void this.loadWindow(start, end, 'prepend').then(() => {
        setTimeout(() => {
          el.scrollTop = prevTop + (el.scrollHeight - prevHeight);
        }, 0);
      });
    } else if (el.scrollHeight - el.scrollTop - el.clientHeight < 200 && windowEnd < total) {
      void this.loadWindow(windowEnd + 1, Math.min(total, windowEnd + this.WINDOW_PAGE), 'append');
    }
  }

  scrollToBottom(smooth = false): void {
    const el = this.logContainerRef?.nativeElement;
    if (!el) return;
    if (smooth) {
      this.followScroll = true;
      el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' });
      this.autoScroll.set(true);
      this.showScrollBtn.set(false);
      return;
    }
    const total = this.totalLines();
    if (this.windowStart + this.windowLines().length - 1 < total) {
      void this.loadWindow(Math.max(1, total - this.WINDOW_PAGE + 1), total, 'replace')
        .then(() => { el.scrollTop = el.scrollHeight; });
    }
    el.scrollTop = el.scrollHeight;
    this.autoScroll.set(true);
    this.showScrollBtn.set(false);
  }

  private scrollToBottomIfAuto(smooth = false): void {
    if (this.autoScroll()) {
      setTimeout(() => this.scrollToBottom(smooth), 0);
    }
  }
}
