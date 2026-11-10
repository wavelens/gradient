/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import type { LogSearchHit } from '@core/models';
import { LogViewerComponent, ServerLog } from './log-viewer.component';
import { NotifyService } from '@core/services/notify.service';

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

async function linkTo(line: number): Promise<void> {
  await TestBed.inject(Router).navigate([], { fragment: `L${line}` });
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

  it('shows a failed line request as an error and loads the tail again on retry', async () => {
    const log = serverLog(50);
    const lines = log.lines.getMockImplementation()!;
    log.lines.mockRejectedValue(new Error('database is down'));
    const { fixture, viewer } = setup({ serverLog: log });
    await settle(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load log');
    expect(root.textContent).toContain('database is down');
    log.lines.mockImplementation(lines);
    (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).find((b) => b.textContent?.includes('Try Again'))?.click();
    await settle(fixture);
    expect(viewer.logLinesRequestError()).toBeNull();
    expect(viewer.windowLines().at(-1)?.n).toBe(50);
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

    it('follows the tail when only the last line grew', async () => {
      const { fixture, internals } = setup();
      await show(fixture, ['a', 'epoch 1']);
      const scroll = vi.spyOn(internals, 'scrollToBottom');
      await show(fixture, ['a', 'epoch 1, epoch 2']);
      expect(scroll).toHaveBeenCalled();
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

    it('marks the matched text and drops the highlight once nothing matches', async () => {
      const { fixture, viewer } = setup();
      await show(fixture, ['compiling', 'error: nope', 'linking']);
      viewer.searchQuery.set('ERROR');
      await viewer.runSearch();
      await settle(fixture);
      const marks = () => Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.log-line-content mark')).map((m) => m.textContent);
      expect(marks()).toEqual(['error']);

      viewer.searchQuery.set('missing');
      await viewer.runSearch();
      await settle(fixture);
      expect(viewer.highlightLine()).toBeNull();
      expect(marks()).toEqual([]);
      expect(fixture.nativeElement.querySelector('.log-line--hl')).toBeNull();
    });

    it('counts matches in lines that stream in after the search', async () => {
      const { fixture, viewer } = setup();
      await show(fixture, ['error: one']);
      viewer.searchQuery.set('error');
      await viewer.runSearch();
      await show(fixture, ['error: one', 'ok', 'error: two']);
      expect(viewer.searchTotal()).toBe(2);
      expect(viewer.searchHits().map((h) => h.line_number)).toEqual([1, 3]);
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

    it('jumps to the next hit without fetching lines it already shows', async () => {
      const log = serverLog(5000, [hit(40), hit(90)]);
      const { fixture, viewer } = setup({ serverLog: log });
      await settle(fixture);
      viewer.searchQuery.set('error');
      await viewer.runSearch();
      await settle(fixture);
      const calls = log.lines.mock.calls.length;
      await viewer.jumpToLine(90);
      expect(log.lines).toHaveBeenCalledTimes(calls);
      expect(viewer.highlightLine()).toBe(90);
    });

    it('sends one search request when Enter follows the typing', async () => {
      vi.useFakeTimers();
      try {
        const log = serverLog(5000, [hit(40)]);
        const search = vi.spyOn(log, 'search');
        const { fixture, viewer } = setup({ serverLog: log });
        fixture.detectChanges();
        viewer.onSearchInput({ target: { value: 'error' } } as unknown as Event);
        void viewer.runSearch();
        void viewer.runSearch();
        await vi.advanceTimersByTimeAsync(400);
        expect(search).toHaveBeenCalledTimes(1);
      } finally {
        vi.useRealTimers();
      }
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

  describe('select all', () => {
    const selectAll = () => new KeyboardEvent('keydown', { key: 'a', ctrlKey: true, cancelable: true });

    it('selects only the log lines once the text cursor is in the log', async () => {
      const { fixture } = setup({ shortcuts: false });
      await show(fixture, ['first', 'second']);
      const lines = fixture.nativeElement.querySelector('.log-lines') as HTMLElement;
      window.getSelection()!.collapse(lines.querySelector('.log-line-content')!, 0);
      const event = selectAll();
      document.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(true);
      expect(window.getSelection()!.toString()).toContain('first');
      expect(window.getSelection()!.toString()).toContain('second');
      expect(window.getSelection()!.getRangeAt(0).commonAncestorContainer).toBe(lines);
    });

    it('leaves select all to the page when the log has neither focus nor the cursor', async () => {
      const { fixture } = setup();
      await show(fixture, ['first']);
      window.getSelection()!.removeAllRanges();
      const event = selectAll();
      document.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(false);
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

    it('opens on the linked line, highlighted, without following the tail', async () => {
      const { fixture, viewer } = setup();
      await linkTo(3000);
      fixture.componentRef.setInput('serverLog', serverLog(5000));
      await settle(fixture);
      expect(viewer.highlightLine()).toBe(3000);
      expect(viewer.autoScroll()).toBe(false);
      expect(viewer.windowLines().some((l) => l.n === 3000)).toBe(true);
    });

    it('uses the linked line for the first log only', async () => {
      const { fixture, viewer } = setup();
      await linkTo(5);
      await show(fixture, numbered(20));
      expect(viewer.highlightLine()).toBe(5);
      await show(fixture, numbered(20, 100));
      expect(viewer.highlightLine()).toBeNull();
      expect(viewer.autoScroll()).toBe(true);
    });
  });

  describe('changing the linked line on an open live log', () => {
    it('jumps to the new line and stays there while lines stream in', async () => {
      const { fixture, viewer } = setup();
      await show(fixture, numbered(580));
      expect(viewer.autoScroll()).toBe(true);

      await linkTo(50);
      await settle(fixture);
      expect(viewer.highlightLine()).toBe(50);
      expect(viewer.autoScroll()).toBe(false);
      expect(Element.prototype.scrollIntoView).toHaveBeenCalled();

      await show(fixture, numbered(600));
      expect(viewer.highlightLine()).toBe(50);
      expect(viewer.autoScroll()).toBe(false);
    });
  });

  it('opens and closes the search from the header button, with the line count beside it', async () => {
    const { fixture, viewer } = setup();
    await show(fixture, numbered(1200));
    const header = (fixture.nativeElement as HTMLElement).querySelector('.log-header')!;
    expect(header.textContent).toContain('1,200 lines');
    const toggle = header.querySelector<HTMLButtonElement>('button[aria-label="Search logs"]')!;
    toggle.click();
    expect(viewer.searchOpen()).toBe(true);
    toggle.click();
    expect(viewer.searchOpen()).toBe(false);
  });

  it('links to a line from its gutter button', async () => {
    const { fixture, viewer } = setup();
    await show(fixture, ['a', 'b']);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const gutter = fixture.nativeElement.querySelectorAll('.log-gutter')[1] as HTMLElement;
    expect(gutter.tagName).toBe('BUTTON');
    expect(gutter.getAttribute('aria-label')).toBe('Link to line 2');
    gutter.click();
    expect(viewer.highlightLine()).toBe(2);
    expect(navigate.mock.calls[0][1]).toMatchObject({ fragment: 'L2' });
  });

  it('says whether copying the line link worked', async () => {
    const { fixture } = setup();
    await show(fixture, ['a', 'b']);
    vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const notify = TestBed.inject(NotifyService);
    const success = vi.spyOn(notify, 'success').mockImplementation(() => undefined);
    const failure = vi.spyOn(notify, 'failure').mockImplementation(() => undefined);
    const writeText = vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('denied'));
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const gutter = fixture.nativeElement.querySelectorAll('.log-gutter')[1] as HTMLElement;
    gutter.click();
    await settle(fixture);
    expect(success).toHaveBeenCalledWith('Link copied');
    gutter.click();
    await settle(fixture);
    expect(failure).toHaveBeenCalledWith("Couldn't copy link", expect.any(Error));
  });

  it('tints every line that starts with the Nix error prefix, also behind color codes', async () => {
    const { fixture } = setup();
    await show(fixture, ['building', '\u001b[31;1merror:\u001b[0m builder failed', '  error: nested', 'no error: here']);
    const tinted = Array.from(fixture.nativeElement.querySelectorAll('.log-line--error')).map((line) => (line as HTMLElement).dataset['line']);
    expect(tinted).toEqual(['2', '3']);
  });

  it('keeps the gutter buttons out of the tab order', async () => {
    const { fixture } = setup();
    await show(fixture, ['a', 'b']);
    const gutters = Array.from(fixture.nativeElement.querySelectorAll('.log-gutter')) as HTMLElement[];
    expect(gutters.every((g) => g.tabIndex === -1)).toBe(true);
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
