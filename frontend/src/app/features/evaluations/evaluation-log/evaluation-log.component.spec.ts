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
import { Evaluation } from '@core/models';

function build(id: string, name: string, status = 'Completed', depth = 0): BuildItem {
  return { id, name, status, has_artefacts: false, updated_at: '', build_time_ms: null, dispatched_job: null, depth };
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
  sortBuilds: (builds: BuildItem[]) => BuildItem[];
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
  // #614: inside a status section the sidebar reads like the dependency graph -
  // the entry point on top, then each dependency layer, alphabetical within one.
  describe('build order', () => {
    it('sorts by dependency layer, then by display name, within a status', () => {
      const { cmp } = setup();
      const sorted = (cmp as unknown as Internals).sortBuilds([
        build('3', 'hash-zlib.drv', 'Completed', 2),
        build('1', 'hash-app.drv', 'Completed', 0),
        build('2', 'hash-openssl.drv', 'Completed', 1),
        build('4', 'hash-acl.drv', 'Completed', 2),
      ]);
      expect(sorted.map(b => b.id)).toEqual(['1', '2', '4', '3']);
    });

    it('keeps status primary: a building dependency stays above a queued dependent', () => {
      const { cmp } = setup();
      const sorted = (cmp as unknown as Internals).sortBuilds([
        build('top', 'hash-app.drv', 'Queued', 0),
        build('dep', 'hash-openssl.drv', 'Building', 1),
      ]);
      expect(sorted.map(b => b.id)).toEqual(['dep', 'top']);
    });

    it('dedups the same derivation arriving under two ids', () => {
      const { cmp } = setup();
      const sorted = (cmp as unknown as Internals).sortBuilds([
        build('a', 'hash-app.drv', 'Completed', 0),
        build('b', 'hash-app.drv', 'Completed', 0),
      ]);
      expect(sorted.length).toBe(1);
    });
  });

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

  // Every per-build action lives in the sidebar's right-click menu, so the model
  // is what decides which of them a given build actually offers.
  describe('build context menu', () => {
    function target(over: Partial<BuildItem> = {}): BuildItem {
      return { ...build('b1', 'hash-hello-1.0.drv'), ...over };
    }

    function open(cmp: EvaluationLogComponent, item: BuildItem) {
      cmp.projectName = 'proj';
      cmp.evaluationId = 'eval-1';
      cmp.contextBuild.set(item);
      const labelled = cmp.buildMenuModel().filter((i) => !i.separator);
      return new Map(labelled.map((i) => [i.label, i]));
    }

    it('offers graph, job, artefacts and log download for a dispatched build', () => {
      const { cmp } = setup();
      const items = open(cmp, target({ dispatched_job: 'job-1', has_artefacts: true }));
      expect([...items.keys()]).toEqual(['Graph', 'Show Job', 'Artefacts', 'Download Log']);
      expect(items.get('Graph')!.routerLink).toEqual(['/project', 'proj', 'graph', 'b1']);
      expect(items.get('Graph')!.queryParams).toEqual({ evalId: 'eval-1' });
      expect(items.get('Show Job')!.routerLink).toEqual(['/board', 'jobs', 'job-1']);
      expect(items.get('Artefacts')!.routerLink).toEqual(['/project', 'proj', 'artefacts', 'b1']);
      expect([...items.values()].some((i) => i.disabled)).toBe(false);
    });

    it('disables the job entry for a build that was never dispatched', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ dispatched_job: null })).get('Show Job')!.disabled).toBe(true);
    });

    it('disables artefacts for a build that published none', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ has_artefacts: false })).get('Artefacts')!.disabled).toBe(true);
    });

    it('disables the log download while the build is still queued', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ status: 'Queued' })).get('Download Log')!.disabled).toBe(true);
    });

    it('opens on a right-click anywhere on the build row, for that row', () => {
      const { fixture, cmp } = setup();
      fixture.detectChanges();
      cmp.evaluation.set({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', trigger: null } as Evaluation);
      cmp.visibleBuilds.set([target({ id: 'b1' }), target({ id: 'b2', dispatched_job: 'job-2' })]);
      fixture.detectChanges();

      const rows = fixture.nativeElement.querySelectorAll('.build-item') as NodeListOf<HTMLElement>;
      rows[1].dispatchEvent(new MouseEvent('contextmenu', { clientX: 9, clientY: 9, bubbles: true, cancelable: true }));
      fixture.detectChanges();

      expect(cmp.contextBuild()!.id).toBe('b2');
      const labels = Array.from(document.querySelectorAll('.gr-menu__item span:last-child')).map((e) => e.textContent);
      expect(labels).toEqual(['Graph', 'Show Job', 'Artefacts', 'Download Log']);
    });

    it('leaves no hover affordance on the row behind', () => {
      const { fixture, cmp } = setup();
      fixture.detectChanges();
      cmp.evaluation.set({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', trigger: null } as Evaluation);
      cmp.visibleBuilds.set([target({ has_artefacts: true })]);
      fixture.detectChanges();

      expect(fixture.nativeElement.querySelectorAll('.build-item a')).toHaveLength(0);
    });
  });

  // The download must serve the complete log, not the virtualized window the
  // page happens to be showing.
  describe('log download', () => {
    const objectUrls = URL as unknown as Record<string, unknown>;

    afterEach(() => {
      delete objectUrls['createObjectURL'];
      delete objectUrls['revokeObjectURL'];
    });

    it('saves the whole log under the build display name', async () => {
      const { cmp } = setup();
      const fetchMock = vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ error: false, message: 'line 1\nline 2\n' }),
      });
      vi.stubGlobal('fetch', fetchMock);
      const created: Blob[] = [];
      objectUrls['createObjectURL'] = (b: Blob) => (created.push(b), 'blob:log');
      const revoke = vi.fn();
      objectUrls['revokeObjectURL'] = revoke;
      const clicked: HTMLAnchorElement[] = [];
      vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
        clicked.push(this);
      });

      await cmp.downloadLog(build('b1', 'hash-hello-1.0.drv'));

      expect(fetchMock).toHaveBeenCalledWith('/api/v1/builds/b1/log', { credentials: 'include' });
      expect(await created[0].text()).toBe('line 1\nline 2\n');
      expect(clicked[0].download).toBe('hello-1.0.log');
      expect(revoke).toHaveBeenCalledWith('blob:log');
    });

    it('saves nothing when the log cannot be read', async () => {
      const { cmp } = setup();
      vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, json: async () => ({}) }));
      const create = vi.fn();
      objectUrls['createObjectURL'] = create;

      await cmp.downloadLog(build('b1', 'hash-hello-1.0.drv'));

      expect(create).not.toHaveBeenCalled();
    });
  });

  // The evaluation page hides the abort behind write access; the log page shows
  // the same evaluation and must not be the way around it.
  describe('abort access', () => {
    it('starts closed, so a view-only visitor is never offered an abort', () => {
      const { cmp } = setup();
      expect(cmp.triggerAccess().canEdit).toBe(false);
    });

    it('opens once the owning task reports the trigger permission', () => {
      const { cmp } = setup();
      cmp.access.set({ managed: false, canEdit: false, canTrigger: true });
      expect(cmp.triggerAccess().canEdit).toBe(true);
    });

    it('stays closed for a member who may only view', () => {
      const { cmp } = setup();
      cmp.access.set({ managed: false, canEdit: true, canTrigger: false });
      expect(cmp.triggerAccess().canEdit).toBe(false);
    });
  });
});
