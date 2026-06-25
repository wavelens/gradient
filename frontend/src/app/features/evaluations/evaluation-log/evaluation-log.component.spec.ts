/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EvaluationLogComponent } from './evaluation-log.component';
import { BuildItem } from '@core/services/evaluations.service';

function build(id: string, name: string, status = 'Completed'): BuildItem {
  return { id, name, status, has_artefacts: false, updated_at: '', build_time_ms: null };
}

function setup(): { fixture: ComponentFixture<EvaluationLogComponent>; cmp: EvaluationLogComponent } {
  TestBed.configureTestingModule({
    imports: [EvaluationLogComponent],
    providers: [provideRouter([]), provideHttpClient(), provideHttpClientTesting()],
  });
  const fixture = TestBed.createComponent(EvaluationLogComponent);
  return { fixture, cmp: fixture.componentInstance };
}

type Internals = {
  appendStreamedLines: (lines: string[]) => void;
  loadWindow: (buildId: string, start: number, end: number, mode: 'replace' | 'append' | 'prepend') => Promise<void>;
  convertAnsiToHtml: (text: string) => string;
  logLines: string[];
  MAX_WINDOW: number;
  LINE_PX: number;
};

describe('EvaluationLogComponent', () => {
  // #341: the live line-count must update on the streaming (Building) render
  // path, identical to chunked (Completed) builds.
  describe('line count', () => {
    it('appendStreamedLines sets logLineCount from logLines', () => {
      const { cmp } = setup();
      (cmp as unknown as Internals).appendStreamedLines(['a', 'b', 'c']);
      expect(cmp.logLineCount()).toBe(3);
    });
  });

  // Streaming logs render through the same virtualized window as chunked logs:
  // per-tick cost must stay O(new lines), never O(total lines).
  describe('streaming virtualization', () => {
    it('converts only the newly streamed lines, not the whole log', () => {
      const { cmp } = setup();
      const c = cmp as unknown as Internals;
      c.appendStreamedLines(Array.from({ length: 100 }, (_, i) => `line ${i}`));
      const spy = vi.spyOn(c, 'convertAnsiToHtml');
      c.appendStreamedLines(['a', 'b', 'c']);
      expect(spy).toHaveBeenCalledTimes(3);
      expect(cmp.logLineCount()).toBe(103);
      expect(cmp.windowLines().length).toBe(103);
      expect(cmp.windowLines()[102].n).toBe(103);
    });

    it('caps the rendered window at MAX_WINDOW, accounting for trimmed lines in the top spacer', () => {
      const { cmp } = setup();
      const c = cmp as unknown as Internals;
      c.appendStreamedLines(Array.from({ length: c.MAX_WINDOW + 500 }, (_, i) => `l${i}`));
      expect(cmp.windowLines().length).toBe(c.MAX_WINDOW);
      expect(cmp.windowLines()[0].n).toBe(501);
      expect(cmp.topSpacerPx()).toBe(500 * c.LINE_PX);
      expect(cmp.logLineCount()).toBe(c.MAX_WINDOW + 500);
    });

    it('keeps the window pinned while scrolled up: new lines only grow the bottom spacer', () => {
      const { cmp } = setup();
      const c = cmp as unknown as Internals;
      c.appendStreamedLines(['a', 'b']);
      cmp.autoScroll.set(false);
      c.appendStreamedLines(['c', 'd', 'e']);
      expect(cmp.windowLines().length).toBe(2);
      expect(cmp.bottomSpacerPx()).toBe(3 * c.LINE_PX);
      expect(cmp.logLineCount()).toBe(5);
    });

    it('pages older lines from in-memory log when scrolled up during streaming', async () => {
      const { cmp } = setup();
      const c = cmp as unknown as Internals;
      c.logLines = Array.from({ length: 2000 }, (_, i) => `l${i + 1}`);
      await c.loadWindow('b1', 1001, 2000, 'replace');
      expect(cmp.windowLines()[0].n).toBe(1001);
      await c.loadWindow('b1', 201, 1000, 'prepend');
      expect(cmp.windowLines()[0].n).toBe(201);
      expect(cmp.windowLines().length).toBe(1800);
      expect(cmp.topSpacerPx()).toBe(200 * c.LINE_PX);
    });
  });

  // #341: sidebar search filters the build list by name without disturbing the
  // status-sorted indices used for keyboard navigation.
  describe('sidebar search', () => {
    it('filters grouped builds by name, case-insensitively', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', '/nix/store/aaa-hello'), build('b', '/nix/store/bbb-world')]);
      cmp.sidebarSearchQuery.set('HELLO');
      const names = cmp.groupedBuilds().flatMap((g) => g.builds.map((x) => x.build.name));
      expect(names).toEqual(['/nix/store/aaa-hello']);
    });

    it('keeps every build when the query is empty', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', 'aaa'), build('b', 'bbb')]);
      cmp.sidebarSearchQuery.set('');
      const names = cmp.groupedBuilds().flatMap((g) => g.builds.map((x) => x.build.name));
      expect(names).toEqual(['aaa', 'bbb']);
    });

    it('preserves visibleBuilds index for matched builds (arrow-nav stays correct)', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', 'aaa'), build('b', 'bbb'), build('c', 'ccc')]);
      cmp.sidebarSearchQuery.set('ccc');
      const indices = cmp.groupedBuilds().flatMap((g) => g.builds.map((x) => x.index));
      expect(indices).toEqual([2]);
    });
  });

  // The builds search bar is hidden until revealed via Ctrl/Cmd+F while the
  // sidebar holds focus, then dismissed with Escape (which also resets the filter).
  describe('sidebar search visibility', () => {
    const key = (init: KeyboardEventInit) => new KeyboardEvent('keydown', { cancelable: true, ...init });

    it('is closed by default', () => {
      const { cmp } = setup();
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('opens on Ctrl+F while the sidebar holds focus, preventing default find', () => {
      const { cmp } = setup();
      cmp.setSidebarFocus(true);
      const ev = key({ key: 'f', ctrlKey: true });
      cmp.onKeydown(ev);
      expect(cmp.sidebarSearchOpen()).toBe(true);
      expect(ev.defaultPrevented).toBe(true);
    });

    it('stays closed on Ctrl+F when the sidebar is not focused', () => {
      const { cmp } = setup();
      cmp.onKeydown(key({ key: 'f', ctrlKey: true }));
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('opens on "/" when not typing in a field', () => {
      const { cmp } = setup();
      cmp.onKeydown(key({ key: '/' }));
      expect(cmp.sidebarSearchOpen()).toBe(true);
    });

    it('ignores "/" typed inside an input', () => {
      const { cmp } = setup();
      const ev = key({ key: '/' });
      Object.defineProperty(ev, 'target', { value: document.createElement('input') });
      cmp.onKeydown(ev);
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('Escape closes the bar and clears the query', () => {
      const { cmp } = setup();
      cmp.openSidebarSearch();
      cmp.sidebarSearchQuery.set('hello');
      cmp.onKeydown(key({ key: 'Escape' }));
      expect(cmp.sidebarSearchOpen()).toBe(false);
      expect(cmp.sidebarSearchQuery()).toBe('');
    });
  });

  // #381: pre-build evals park with an eval_workers reason naming the missing
  // capability (fetch while Fetching, eval while Evaluating).
  describe('eval_workers waiting reason', () => {
    it('titles and explains a missing fetch worker', () => {
      const { cmp } = setup();
      const reason = { kind: 'eval_workers', capability: 'fetch', connected_workers: 0 } as const;
      expect(cmp.waitingTitle(reason)).toBe('Waiting for a Fetch Worker');
      expect(cmp.formatWaitingReason(reason)).toContain('fetch the flake sources');
    });

    it('titles and explains a missing eval worker with connected count', () => {
      const { cmp } = setup();
      const reason = { kind: 'eval_workers', capability: 'eval', connected_workers: 2 } as const;
      expect(cmp.waitingTitle(reason)).toBe('Waiting for an Eval Worker');
      expect(cmp.formatWaitingReason(reason)).toBe('2 workers are connected, but none can run the evaluation.');
    });

    it('titles a full-cache stall', () => {
      const { cmp } = setup();
      expect(cmp.waitingTitle({ kind: 'cache_storage_full' })).toBe('Cache Storage Full');
    });

    it('titles and explains a graph-stuck stall', () => {
      const { cmp } = setup();
      const reason = { kind: 'graph_stuck', pending_anchors: 9 } as const;
      expect(cmp.waitingTitle(reason)).toBe('Recovering Build Graph');
      expect(cmp.formatWaitingReason(reason)).toBe(
        'Workers are available, but 9 builds are blocked on dependencies. Recovering automatically.',
      );
    });
  });
});
