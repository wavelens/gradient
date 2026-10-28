/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import type { LogSearchHit } from '@core/models';
import { LogViewerComponent, ServerLog } from './log-viewer.component';

type Internals = {
  renderLine: (text: string, n: number) => unknown;
  loadWindow: (start: number, end: number, mode: 'replace' | 'append' | 'prepend') => Promise<void>;
  scrollToBottom: (smooth?: boolean) => void;
  logContainerRef: { nativeElement: Partial<HTMLElement> } | undefined;
  MAX_WINDOW: number;
  LINE_PX: number;
};

function setup(inputs: Record<string, unknown> = {}) {
  TestBed.configureTestingModule({ imports: [LogViewerComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(LogViewerComponent);
  for (const [name, value] of Object.entries(inputs)) fixture.componentRef.setInput(name, value);
  fixture.detectChanges();
  const viewer = fixture.componentInstance;
  return { fixture, viewer, internals: viewer as unknown as Internals };
}

async function settle(fixture: ComponentFixture<LogViewerComponent>): Promise<void> {
  for (let pass = 0; pass < 2; pass++) await new Promise((resolve) => setTimeout(resolve));
  fixture.detectChanges();
}

async function show(fixture: ComponentFixture<LogViewerComponent>, lines: readonly string[]): Promise<void> {
  fixture.componentRef.setInput('lines', lines);
  fixture.detectChanges();
  await settle(fixture);
}

const numbered = (count: number, from = 1) => Array.from({ length: count }, (_, i) => `l${from + i}`);

function serverLog(total: number, hits: LogSearchHit[] = [], totalMatches = hits.length): ServerLog & { lines: ReturnType<typeof vi.fn> } {
  return {
    total,
    lines: vi.fn(async (start: number, end: number) => numbered(end - start + 1, start)),
    search: async (_query, onHits) => {
      onHits(hits);
      return totalMatches;
    },
  };
}

const hit = (line_number: number): LogSearchHit => ({ line_number, chunk_index: 0, byte_offset: 0, preview: '' });

describe('LogViewerComponent', () => {
  beforeEach(() => {
    Element.prototype.scrollIntoView = vi.fn();
    Element.prototype.scrollTo = vi.fn() as unknown as typeof Element.prototype.scrollTo;
  });

  // Per-tick cost of a streaming log must stay O(new lines), never O(total lines).
  describe('streaming virtualization', () => {
    it('renders only the newly streamed lines, not the whole log', async () => {
      const { fixture, viewer, internals } = setup();
      const log = numbered(100);
      await show(fixture, log);
      const render = vi.spyOn(internals, 'renderLine');
      await show(fixture, [...log, 'a', 'b', 'c']);
      expect(render).toHaveBeenCalledTimes(3);
      expect(viewer.lineCount()).toBe(103);
      expect(viewer.windowLines().length).toBe(103);
      expect(viewer.windowLines()[102].n).toBe(103);
    });

    it('caps the rendered window at MAX_WINDOW, accounting for trimmed lines in the top spacer', async () => {
      const { fixture, viewer, internals } = setup();
      await show(fixture, ['first']);
      await show(fixture, ['first', ...numbered(internals.MAX_WINDOW + 500, 2)]);
      expect(viewer.windowLines().length).toBe(internals.MAX_WINDOW);
      expect(viewer.windowLines()[0].n).toBe(502);
      expect(viewer.topSpacerPx()).toBe(501 * internals.LINE_PX);
      expect(viewer.lineCount()).toBe(internals.MAX_WINDOW + 501);
    });

    it('keeps the window pinned while scrolled up: new lines only grow the bottom spacer', async () => {
      const { fixture, viewer, internals } = setup();
      await show(fixture, ['a', 'b']);
      viewer.autoScroll.set(false);
      await show(fixture, ['a', 'b', 'c', 'd', 'e']);
      expect(viewer.windowLines().length).toBe(2);
      expect(viewer.bottomSpacerPx()).toBe(3 * internals.LINE_PX);
      expect(viewer.lineCount()).toBe(5);
    });

    it('pages older lines from the in-memory log when scrolled up', async () => {
      const { fixture, viewer, internals } = setup();
      await show(fixture, numbered(2000));
      expect(viewer.windowLines()[0].n).toBe(1201);
      await internals.loadWindow(201, 1200, 'prepend');
      expect(viewer.windowLines()[0].n).toBe(201);
      expect(viewer.windowLines().length).toBe(1800);
      expect(viewer.topSpacerPx()).toBe(200 * internals.LINE_PX);
    });

    it('updates a last line that grew, keeping the search open', async () => {
      const { fixture, viewer } = setup();
      await show(fixture, ['a', 'epoch 1']);
      viewer.openSearch();
      await show(fixture, ['a', 'epoch 2', 'b']);
      expect(viewer.windowLines().map((l) => l.n)).toEqual([1, 2, 3]);
      expect(String(viewer.windowLines()[1].html)).toContain('epoch 2');
      expect(viewer.searchOpen()).toBe(true);
    });

    it('starts over when the lines are replaced by another log', async () => {
      const { fixture, viewer } = setup();
      await show(fixture, ['a', 'b', 'c']);
      viewer.openSearch();
      await show(fixture, ['x']);
      expect(viewer.searchOpen()).toBe(false);
      expect(viewer.windowLines().map((l) => l.n)).toEqual([1]);
    });
  });

  // #636: a live log follows the tail with a smooth scroll, rather than jumping
  // one line every tick.
  describe('following a live log', () => {
    const scrolled = (scrollTop: number) =>
      ({ target: { scrollHeight: 1000, scrollTop, clientHeight: 200 } }) as unknown as Event;

    it('keeps following while its own smooth scroll is still on the way down', () => {
      const { viewer, internals } = setup();
      const scrollTo = vi.fn();
      internals.logContainerRef = { nativeElement: { scrollHeight: 1000, scrollTo } as Partial<HTMLElement> };
      internals.scrollToBottom(true);
      expect(scrollTo).toHaveBeenCalledWith({ top: 1000, behavior: 'smooth' });
      viewer.onLogScroll(scrolled(300));
      expect(viewer.autoScroll()).toBe(true);
    });

    it('stops following once the reader scrolls up', () => {
      const { viewer, internals } = setup();
      internals.logContainerRef = { nativeElement: { scrollHeight: 1000, scrollTo: vi.fn() } as Partial<HTMLElement> };
      internals.scrollToBottom(true);
      viewer.onLogWheel(new WheelEvent('wheel', { deltaY: -40 }));
      viewer.onLogScroll(scrolled(300));
      expect(viewer.autoScroll()).toBe(false);
      expect(viewer.showScrollBtn()).toBe(true);
    });
  });

  describe('search', () => {
    it('searches the lines in memory while the log streams', async () => {
      const { fixture, viewer } = setup();
      await show(fixture, ['compiling', 'error: nope', 'linking']);
      viewer.searchQuery.set('error');
      await viewer.runSearch();
      await settle(fixture);
      expect(viewer.searchHits().map((h) => h.line_number)).toEqual([2]);
      expect(viewer.searchTotal()).toBe(1);
      expect(viewer.highlightLine()).toBe(2);
    });

    it('asks the server for a finished log and jumps to the first hit', async () => {
      const { fixture, viewer } = setup({ serverLog: serverLog(5000, [hit(40), hit(90)], 7) });
      await settle(fixture);
      viewer.searchQuery.set('error');
      await viewer.runSearch();
      await settle(fixture);
      expect(viewer.searchHits().map((h) => h.line_number)).toEqual([40, 90]);
      expect(viewer.searchTotal()).toBe(7);
      expect(viewer.currentHit()).toBe(0);
      expect(viewer.highlightLine()).toBe(40);
    });

    it('waits for three characters before searching', async () => {
      vi.useFakeTimers();
      try {
        const { fixture, viewer } = setup({ lines: ['error'] });
        fixture.detectChanges();
        viewer.onSearchInput({ target: { value: 'er' } } as unknown as Event);
        vi.advanceTimersByTime(400);
        expect(viewer.searchLoading()).toBe(false);
        viewer.onSearchInput({ target: { value: 'err' } } as unknown as Event);
        expect(viewer.searchLoading()).toBe(true);
        vi.advanceTimersByTime(400);
        expect(viewer.searchHits().map((h) => h.line_number)).toEqual([1]);
      } finally {
        vi.useRealTimers();
      }
    });

    it('opens on Ctrl+F, preventing the browser find and closes on Escape', () => {
      const { fixture, viewer } = setup({ lines: ['a'] });
      const find = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true });
      document.dispatchEvent(find);
      fixture.detectChanges();
      expect(find.defaultPrevented).toBe(true);
      expect(fixture.nativeElement.querySelector('.log-search-input')).not.toBeNull();
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
      expect(viewer.searchOpen()).toBe(false);
    });

    it('leaves Ctrl+F to the page when its shortcuts are off', () => {
      const { viewer } = setup({ shortcuts: false });
      const find = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true });
      document.dispatchEvent(find);
      expect(find.defaultPrevented).toBe(false);
      expect(viewer.searchOpen()).toBe(false);
    });
  });

  describe('opening a finished log', () => {
    it('loads only the tail window from the server', async () => {
      const log = serverLog(5000);
      const { fixture, viewer, internals } = setup({ serverLog: log });
      await settle(fixture);
      expect(log.lines).toHaveBeenCalledWith(4201, 5000);
      expect(viewer.windowLines()[0].n).toBe(4201);
      expect(viewer.topSpacerPx()).toBe(4200 * internals.LINE_PX);
      expect(viewer.autoScroll()).toBe(true);
    });

    it('opens on the initial line, highlighted, without following the tail', async () => {
      const { fixture, viewer } = setup({ serverLog: serverLog(5000), initialLine: 3000 });
      await settle(fixture);
      expect(viewer.highlightLine()).toBe(3000);
      expect(viewer.autoScroll()).toBe(false);
      expect(viewer.windowLines().some((l) => l.n === 3000)).toBe(true);
    });
  });

  it('links to a line from its gutter', async () => {
    const { fixture, viewer } = setup();
    await show(fixture, ['a', 'b']);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    (fixture.nativeElement.querySelectorAll('.log-gutter')[1] as HTMLElement).click();
    expect(viewer.highlightLine()).toBe(2);
    expect(navigate.mock.calls[0][1]).toMatchObject({ fragment: 'L2' });
  });

  it('renders a marker line as a separator carrying its name', async () => {
    const { fixture } = setup({ marker: (line: string) => (line.startsWith('# ') ? line.slice(2) : null) });
    await show(fixture, ['before', '# epoch-3', 'after']);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('[role="separator"]')?.textContent?.trim()).toBe('epoch-3');
    expect(root.textContent).not.toContain('# epoch-3');
    expect(root.textContent).toContain('after');
  });
});
