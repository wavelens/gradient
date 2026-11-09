/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { HttpHeaders, HttpResponse, provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EvaluationLogComponent } from './evaluation-log.component';
import { EvaluationsService } from '@core/services/evaluations.service';
import { LiveEvent, LiveService } from '@core/services/live.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { EMPTY, NEVER, type Observable, Subject, map, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { NotifyService } from '@core/services/notify.service';
import { UserInfoResponse, BuildItem, EvaluationResponse, EvaluationStatus } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import type { LogViewerComponent } from '@shared/log';

function build(id: string, name: string, status = 'Completed', depth = 0): BuildItem {
  return { id, name, status, has_artefacts: false, updated_at: '', build_time_ms: null, build_started_at: null, dispatched_job: null, depth, prioritized: false, ifd: false };
}

function setup(): {
  fixture: ComponentFixture<EvaluationLogComponent>;
  cmp: EvaluationLogComponent;
  confirm: ReturnType<typeof vi.fn>;
} {
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [EvaluationLogComponent],
    providers: [provideRouter([]), provideHttpClient(), provideHttpClientTesting(), { provide: ConfirmService, useValue: { confirm } }],
  });
  const fixture = TestBed.createComponent(EvaluationLogComponent);
  return { fixture, cmp: fixture.componentInstance, confirm };
}

type Internals = {
  sortBuilds: (builds: BuildItem[]) => BuildItem[];
  appendStreamedLines: (lines: string[]) => void;
  fetchInitialLogs: (id: string) => Promise<void>;
};

function fakeViewer(cmp: EvaluationLogComponent) {
  const viewer = { searchOpen: signal(false), openSearch: vi.fn(), closeSearch: vi.fn() };
  cmp.logViewer = viewer as unknown as LogViewerComponent;
  return viewer;
}

describe('EvaluationLogComponent', () => {
  // #341: the live line-count must update on the streaming (Building) render
  // path, identical to chunked (Completed) builds.
  describe('streamed lines', () => {
    it('extend the lines handed to the viewer and the line count', () => {
      const { cmp } = setup();
      const c = cmp as unknown as Internals;
      c.appendStreamedLines(['a', 'b']);
      c.appendStreamedLines(['c']);
      expect(cmp.logLines()).toEqual(['a', 'b', 'c']);
      expect(cmp.logLineCount()).toBe(3);
    });

    it('stay paused after a rate limit note until the worker resumes the log', () => {
      const { cmp } = setup();
      const c = cmp as unknown as Internals;
      c.appendStreamedLines(['a', '\x1b[0m[gradient: log paused, the build can write output faster than the rate limit for the log]']);
      expect(cmp.logPaused()).toBe(true);
      c.appendStreamedLines(['still building']);
      expect(cmp.logPaused()).toBe(true);
      c.appendStreamedLines(['\x1b[0m[gradient: log resumed, the rate limit for the log dropped 12 lines]', 'b']);
      expect(cmp.logPaused()).toBe(false);
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

    it('Sort the builds with status DependencyFailed directly after failed builds.', () => {
      const { cmp } = setup();
      const sorted = (cmp as unknown as Internals).sortBuilds([
        build('skipped', 'hash-docs.drv', 'Skipped', 0),
        build('dependent', 'hash-app.drv', 'DependencyFailed', 0),
        build('done', 'hash-zlib.drv', 'Completed', 1),
        build('failed', 'hash-openssl.drv', 'FailedPermanent', 1),
      ]);
      expect(sorted.map(b => b.id)).toEqual(['failed', 'dependent', 'done', 'skipped']);
    });

    it('dedups the same derivation arriving under two ids', () => {
      const { cmp } = setup();
      const sorted = (cmp as unknown as Internals).sortBuilds([
        build('a', 'hash-app.drv', 'Completed', 0),
        build('b', 'hash-app.drv', 'Completed', 0),
      ]);
      expect(sorted.length).toBe(1);
    });

    it('sorts numbers in names by value', () => {
      const { cmp } = setup();
      const sorted = (cmp as unknown as Internals).sortBuilds([
        build('10', 'hash-pkg-10.drv'),
        build('2', 'hash-pkg-2.drv'),
        build('106', 'hash-pkg-106.drv'),
      ]);
      expect(sorted.map(b => b.id)).toEqual(['2', '10', '106']);
    });
  });

  describe('builds drawer', () => {
    it('closes once a build or the evaluation is picked', () => {
      const { cmp } = setup();
      const b = build('a', 'aaa');
      cmp.visibleBuilds.set([b]);

      cmp.buildsOpen.set(true);
      cmp.selectBuild(b, true);
      expect(cmp.buildsOpen()).toBe(false);

      cmp.buildsOpen.set(true);
      cmp.selectBuild(b, true);
      expect(cmp.buildsOpen()).toBe(false);

      cmp.buildsOpen.set(true);
      cmp.selectEvaluationSection();
      expect(cmp.buildsOpen()).toBe(false);
    });
  });

  describe('sidebar search', () => {
    it('filters grouped builds by name, case-insensitively', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', '/nix/store/aaa-hello'), build('b', '/nix/store/bbb-world')]);
      cmp.sidebarSearchQuery.set('HELLO');
      const names = cmp.groupedBuilds().flatMap((g) => g.builds.map((b) => b.name));
      expect(names).toEqual(['/nix/store/aaa-hello']);
    });

    it('keeps every build when the query is empty', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', 'aaa'), build('b', 'bbb')]);
      cmp.sidebarSearchQuery.set('');
      const names = cmp.groupedBuilds().flatMap((g) => g.builds.map((b) => b.name));
      expect(names).toEqual(['aaa', 'bbb']);
    });

    it('arrow navigation skips builds the query filters out', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', 'app'), build('b', 'bbb'), build('c', 'apt')]);
      cmp.sidebarSearchQuery.set('ap');
      cmp.selectAdjacentBuild('a', 1);
      expect(cmp.selectedBuildId()).toBe('c');
    });
  });

  // #676: status sections collapse from their header; a collapsed section keeps
  // its count but hides its builds from the list and from arrow navigation.
  describe('collapsible sections', () => {
    const builds = () => [build('b1', 'b1', 'Building'), build('f1', 'f1', 'Failed'), build('c1', 'c1', 'Completed')];

    it('Add builds of unknown status to the Other section.', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('c1', 'c1', 'Completed'), build('n1', 'n1', 'Rebuilding')]);
      expect(cmp.groupedBuilds().map((g) => [g.key, g.builds.map((b) => b.id)])).toEqual([['completed', ['c1']], ['other', ['n1']]]);
    });

    it('labels substituted builds as substituted and gives every finished build its duration', () => {
      const { cmp } = setup();
      const timedOut = { ...build('t1', 't1', 'FailedTimeout'), build_time_ms: 287 };
      cmp.visibleBuilds.set([build('c1', 'c1', 'Completed'), build('s1', 's1', 'Substituted'), timedOut]);
      expect(cmp.groupedBuilds().map((g) => g.label)).toEqual(['Failed', 'Completed', 'Substituted']);
      expect(cmp.getBuildElapsed(timedOut)).toBe('287 ms');
    });

    it('starts with every section expanded', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set(builds());
      expect(cmp.groupedBuilds().map((g) => g.collapsed)).toEqual([false, false, false]);
    });

    it('toggling a section collapses it and keeps its count', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set(builds());
      cmp.toggleGroup('failed');
      const failed = cmp.groupedBuilds().find((g) => g.key === 'failed')!;
      expect(failed.collapsed).toBe(true);
      expect(failed.builds.length).toBe(1);
      cmp.toggleGroup('failed');
      expect(cmp.groupedBuilds().find((g) => g.key === 'failed')!.collapsed).toBe(false);
    });

    it('arrow navigation skips builds in a collapsed section', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set(builds());
      cmp.toggleGroup('failed');
      cmp.selectAdjacentBuild('b1', 1);
      expect(cmp.selectedBuildId()).toBe('c1');
      cmp.selectAdjacentBuild('c1', -1);
      expect(cmp.selectedBuildId()).toBe('b1');
    });
  });

  // Ctrl/Cmd+F opens the search of the region under the pointer, falling back to
  // keyboard focus; Escape dismisses the bar and resets the filter.
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

    it('opens on Ctrl+F while no log is open, so the browser find never shows', () => {
      const { cmp } = setup();
      const ev = key({ key: 'f', ctrlKey: true });
      cmp.onKeydown(ev);
      expect(cmp.sidebarSearchOpen()).toBe(true);
      expect(ev.defaultPrevented).toBe(true);
    });

    it('opens the log search instead while a log is open, streaming or not', () => {
      const { cmp } = setup();
      const viewer = fakeViewer(cmp);
      cmp.selectedBuildId.set('b1');
      const ev = key({ key: 'f', ctrlKey: true });
      cmp.onKeydown(ev);
      expect(viewer.openSearch).toHaveBeenCalled();
      expect(cmp.sidebarSearchOpen()).toBe(false);
      expect(ev.defaultPrevented).toBe(true);
    });

    it('prefers the builds search while the sidebar holds focus, even with a log open', () => {
      const { cmp } = setup();
      const viewer = fakeViewer(cmp);
      cmp.selectedBuildId.set('b1');
      cmp.setSidebarFocus(true);
      cmp.onKeydown(key({ key: 'f', ctrlKey: true }));
      expect(cmp.sidebarSearchOpen()).toBe(true);
      expect(viewer.openSearch).not.toHaveBeenCalled();
    });

    it('opens the builds search while the pointer is over the sidebar, even with a log open', () => {
      const { cmp } = setup();
      const viewer = fakeViewer(cmp);
      cmp.selectedBuildId.set('b1');
      cmp.setPointerOver('builds');
      cmp.onKeydown(key({ key: 'f', ctrlKey: true }));
      expect(cmp.sidebarSearchOpen()).toBe(true);
      expect(viewer.openSearch).not.toHaveBeenCalled();
    });

    it('opens the log search while the pointer is over the log, even with focus left in the sidebar', () => {
      const { cmp } = setup();
      const viewer = fakeViewer(cmp);
      cmp.selectedBuildId.set('b1');
      cmp.setSidebarFocus(true);
      cmp.setPointerOver('log');
      cmp.onKeydown(key({ key: 'f', ctrlKey: true }));
      expect(viewer.openSearch).toHaveBeenCalled();
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('closes the builds search before the log search on Escape', () => {
      const { cmp } = setup();
      const viewer = fakeViewer(cmp);
      viewer.searchOpen.set(true);
      cmp.selectedBuildId.set('b1');
      cmp.openSidebarSearch();
      cmp.onKeydown(key({ key: 'Escape' }));
      expect(cmp.sidebarSearchOpen()).toBe(false);
      expect(viewer.closeSearch).not.toHaveBeenCalled();
      cmp.onKeydown(key({ key: 'Escape' }));
      expect(viewer.closeSearch).toHaveBeenCalled();
    });

    it('leaves "/" to the header search', () => {
      const { cmp } = setup();
      const ev = key({ key: '/' });
      cmp.onKeydown(ev);
      expect(cmp.sidebarSearchOpen()).toBe(false);
      expect(ev.defaultPrevented).toBe(false);
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

    it('passes the task and the evaluation so build pages keep both breadcrumbs', () => {
      const { cmp } = setup();
      cmp.evaluation.set({ id: 'eval-1', task_name: 'ci' } as EvaluationResponse);
      const items = open(cmp, target({ dispatched_job: 'job-1', has_artefacts: true }));
      expect(String(items.get('Dependency graph')!.routerLink)).toBe('/project/proj/graph/b1?task=ci&eval=eval-1');
      expect(String(items.get('Closure')!.routerLink)).toBe('/project/proj/closure/build/b1?task=ci&eval=eval-1');
      expect(String(items.get('Artefacts')!.routerLink)).toBe('/project/proj/artefacts/b1?task=ci&eval=eval-1');
    });

    it('offers graph, job, artefacts and log download for an assigned build', () => {
      const { cmp } = setup();
      vi.spyOn(TestBed.inject(AuthService), 'isAuthenticated').mockReturnValue(true);
      const items = open(cmp, target({ dispatched_job: 'job-1', has_artefacts: true }));
      expect([...items.keys()]).toEqual(['Dependency graph', 'Closure', 'Show job', 'Artefacts', 'Download Log', 'SBOM']);
      expect(String(items.get('Dependency graph')!.routerLink)).toBe('/project/proj/graph/b1?eval=eval-1');
      expect(String(items.get('Closure')!.routerLink)).toBe('/project/proj/closure/build/b1?eval=eval-1');
      expect(String(items.get('Show job')!.routerLink)).toBe('/board/jobs/job-1');
      expect(String(items.get('Artefacts')!.routerLink)).toBe('/project/proj/artefacts/b1?eval=eval-1');
      expect([...items.values()].some((i) => i.disabled)).toBe(false);
    });

    it('disables the closure until the build output exists', () => {
      const { cmp } = setup();
      const closure = open(cmp, target({ status: 'Failed' })).get('Closure')!;
      expect(closure.disabled).toBe(true);
      expect(closure.routerLink).toBeUndefined();
    });

    it('disables the job entry for a build that was never assigned', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ dispatched_job: null })).get('Show job')!.disabled).toBe(true);
    });

    it('disables the job entry for a signed-out visitor, since the job page needs a sign-in', () => {
      const { cmp } = setup();
      vi.spyOn(TestBed.inject(AuthService), 'isAuthenticated').mockReturnValue(false);
      expect(open(cmp, target({ dispatched_job: 'job-1' })).get('Show job')!.disabled).toBe(true);
    });

    it('disables artefacts for a build that published none', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ has_artefacts: false })).get('Artefacts')!.disabled).toBe(true);
    });

    it('disables the log download while the build is still queued', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ status: 'Queued' })).get('Download Log')!.disabled).toBe(true);
    });

    it('greys out the sbom until the build has outputs', () => {
      const { cmp } = setup();
      expect(open(cmp, target({ status: 'Building' })).get('SBOM')!.disabled).toBe(true);
      expect(open(cmp, target({ status: 'Failed' })).get('SBOM')!.disabled).toBe(true);
      expect(open(cmp, target({ status: 'Substituted' })).get('SBOM')!.disabled).toBe(false);
    });

    it('downloads the sbom of the build under the server filename', () => {
      const { cmp } = setup();
      const download = vi.spyOn(TestBed.inject(EvaluationsService), 'downloadBuildSbom').mockReturnValue(of(
        new HttpResponse({
          body: new Blob(['{}']),
          headers: new HttpHeaders({ 'content-disposition': 'attachment; filename="hello-1.0-sbom.cdx.json"' }),
        }),
      ));
      const objectUrls = URL as unknown as Record<string, unknown>;
      objectUrls['createObjectURL'] = () => 'blob:sbom';
      objectUrls['revokeObjectURL'] = () => undefined;
      const clicked: HTMLAnchorElement[] = [];
      vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
        clicked.push(this);
      });

      open(cmp, target()).get('SBOM')!.command!();

      expect(download).toHaveBeenCalledWith('b1');
      expect(clicked[0].download).toBe('hello-1.0-sbom.cdx.json');
      delete objectUrls['createObjectURL'];
      delete objectUrls['revokeObjectURL'];
    });

    it('opens on a right-click anywhere on the build row, for that row', () => {
      const { fixture, cmp } = setup();
      fixture.detectChanges();
      cmp.evaluation.set({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', trigger: null } as EvaluationResponse);
      cmp.visibleBuilds.set([target({ id: 'b1' }), target({ id: 'b2', dispatched_job: 'job-2' })]);
      fixture.detectChanges();

      expect(fixture.nativeElement.querySelector('.sidebar-builds-heading')?.textContent?.trim()).toBe('Builds');
      const rows = fixture.nativeElement.querySelectorAll('.build-item') as NodeListOf<HTMLElement>;
      rows[1].dispatchEvent(new MouseEvent('contextmenu', { clientX: 9, clientY: 9, bubbles: true, cancelable: true }));
      fixture.detectChanges();

      expect(cmp.contextBuild()!.id).toBe('b2');
      const labels = Array.from(document.querySelectorAll('.gr-menu__item span:last-child')).map((e) => e.textContent);
      expect(labels).toEqual(['Dependency graph', 'Closure', 'Show job', 'Artefacts', 'Download Log', 'SBOM']);
    });

    it('opens the same menu from the row\'s three dots, without selecting the row', () => {
      const { fixture, cmp } = setup();
      fixture.detectChanges();
      cmp.evaluation.set({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', trigger: null } as EvaluationResponse);
      cmp.visibleBuilds.set([target({ id: 'b1' }), target({ id: 'b2' })]);
      fixture.detectChanges();
      const select = vi.spyOn(cmp, 'selectBuild');

      const kebabs = fixture.nativeElement.querySelectorAll('.build-item button.sidebar-kebab') as NodeListOf<HTMLButtonElement>;
      expect(kebabs).toHaveLength(2);
      expect(kebabs[1].getAttribute('aria-label')).toBe('Actions for hello-1.0');
      kebabs[1].click();
      fixture.detectChanges();

      expect(cmp.contextBuild()!.id).toBe('b2');
      expect(select).not.toHaveBeenCalled();
      expect(document.querySelectorAll('.gr-menu__item').length).toBeGreaterThan(0);
    });

    it('selects a build from a keyboard button that holds no other control', () => {
      const { fixture, cmp } = setup();
      vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}', { status: 404 }));
      fixture.detectChanges();
      cmp.evaluation.set({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', trigger: null } as EvaluationResponse);
      cmp.visibleBuilds.set([target({ id: 'b1' })]);
      fixture.detectChanges();

      const select = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.build-item button.sidebar-row-link')!;
      expect(select.getAttribute('aria-label')).toBe('hello-1.0');
      expect(select.querySelector('button')).toBeNull();
      select.click();
      expect(cmp.selectedBuildId()).toBe('b1');
    });

    describe('prioritize', () => {
      function withTrigger(cmp: EvaluationLogComponent): EvaluationLogComponent {
        cmp.access.set({ managed: false, canEdit: false, canTrigger: true });
        return cmp;
      }

      function asSuperuser(cmp: EvaluationLogComponent): EvaluationLogComponent {
        vi.spyOn(TestBed.inject(AuthService), 'user').mockReturnValue({ superuser: true } as UserInfoResponse);
        return cmp;
      }

      it('offers prioritize for a pending build to a superuser', () => {
        const { cmp } = setup();
        expect(open(asSuperuser(cmp), target({ status: 'Queued' })).has('Prioritize')).toBe(true);
      });

      it('hides prioritize from a member who may trigger but is not a superuser', () => {
        const { cmp } = setup();
        expect(open(withTrigger(cmp), target({ status: 'Queued' })).has('Prioritize')).toBe(false);
      });

      it('hides prioritize from a view-only visitor', () => {
        const { cmp } = setup();
        expect(open(cmp, target({ status: 'Queued' })).has('Prioritize')).toBe(false);
      });

      it('hides prioritize once the build is already prioritized', () => {
        const { cmp } = setup();
        expect(open(asSuperuser(cmp), target({ status: 'Queued', prioritized: true })).has('Prioritize')).toBe(false);
      });

      it('hides prioritize for a finished build', () => {
        const { cmp } = setup();
        for (const status of ['Completed', 'Substituted', 'FailedPermanent', 'DependencyFailed', 'Skipped']) {
          expect(open(asSuperuser(cmp), target({ status })).has('Prioritize')).toBe(false);
        }
      });

      it('prioritizes the build, then reloads the build list', () => {
        const { cmp } = setup();
        const prioritize = vi.spyOn(TestBed.inject(EvaluationsService), 'prioritizeBuild').mockReturnValue(of('Success'));
        vi.spyOn(TestBed.inject(EvaluationsService), 'getEvaluation')
          .mockReturnValue(of({ id: 'eval-1', status: 'Building', created_at: '2026-01-01T00:00:00' } as EvaluationResponse));
        const reload = vi.spyOn(cmp, 'loadBuilds').mockImplementation(() => {});
        open(asSuperuser(cmp), target({ status: 'Building' })).get('Prioritize')!.command!();
        expect(prioritize).toHaveBeenCalledWith('b1');
        expect(reload).toHaveBeenCalled();
      });
    });

    describe('abort and retry', () => {
      function inEvaluation(cmp: EvaluationLogComponent, status: EvaluationStatus, canTrigger = true): EvaluationLogComponent {
        cmp.access.set({ managed: false, canEdit: false, canTrigger });
        cmp.evaluation.set({ id: 'eval-1', status, created_at: '2026-01-01T00:00:00', trigger: null } as EvaluationResponse);
        return cmp;
      }

      function reloadedAs(cmp: EvaluationLogComponent, status: EvaluationStatus) {
        vi.spyOn(TestBed.inject(EvaluationsService), 'getEvaluation')
          .mockReturnValue(of({ ...cmp.evaluation()!, status }));
        return vi.spyOn(cmp, 'loadBuilds').mockImplementation(() => {});
      }

      it('offers abort for a created, queued or building build', () => {
        const { cmp } = setup();
        inEvaluation(cmp, 'Building');
        for (const status of ['Created', 'Queued', 'Building']) {
          expect(open(cmp, target({ status })).has('Abort')).toBe(true);
        }
        for (const status of ['Completed', 'FailedPermanent', 'Aborted']) {
          expect(open(cmp, target({ status })).has('Abort')).toBe(false);
        }
      });

      it('offers retry for a failed, aborted or dependency-failed build', () => {
        const { cmp } = setup();
        inEvaluation(cmp, 'Building');
        for (const status of ['FailedPermanent', 'FailedTimeout', 'Aborted', 'DependencyFailed']) {
          expect(open(cmp, target({ status })).has('Retry')).toBe(true);
        }
        for (const status of ['Queued', 'Building', 'Completed']) {
          expect(open(cmp, target({ status })).has('Retry')).toBe(false);
        }
      });

      it('hides abort and retry from a view-only visitor', () => {
        const { cmp } = setup();
        inEvaluation(cmp, 'Building', false);
        expect(open(cmp, target({ status: 'Building' })).has('Abort')).toBe(false);
        expect(open(cmp, target({ status: 'FailedPermanent' })).has('Retry')).toBe(false);
      });

      it('keeps retry but not abort once the evaluation failed or was aborted', () => {
        const { cmp } = setup();
        for (const status of ['Failed', 'Aborted'] as const) {
          inEvaluation(cmp, status);
          expect(open(cmp, target({ status: 'Queued' })).has('Abort')).toBe(false);
          expect(open(cmp, target({ status: 'DependencyFailed' })).has('Retry')).toBe(true);
        }
      });

      it('hides retry in a failed evaluation a newer evaluation replaced', () => {
        const { cmp } = setup();
        inEvaluation(cmp, 'Failed');
        cmp.evaluation.update(evaluation => ({ ...evaluation!, next: 'eval-2' }));
        expect(open(cmp, target({ status: 'FailedPermanent' })).has('Retry')).toBe(false);
      });

      it('hides retry in a completed evaluation', () => {
        const { cmp } = setup();
        inEvaluation(cmp, 'Completed');
        expect(open(cmp, target({ status: 'FailedPermanent' })).has('Retry')).toBe(false);
      });

      it('asks first, naming the build, then aborts it and reloads the build list', () => {
        const { cmp, confirm } = setup();
        const abort = vi.spyOn(TestBed.inject(EvaluationsService), 'abortBuild').mockReturnValue(of('Success'));
        inEvaluation(cmp, 'Building');
        const reload = reloadedAs(cmp, 'Building');
        const build = target({ status: 'Building' });
        open(cmp, build).get('Abort')!.command!();
        expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Abort Build', target: expect.stringContaining(cmp.buildDisplayName(build.name)) }));
        expect(abort).toHaveBeenCalledWith('b1');
        expect(reload).toHaveBeenCalled();
      });

      it('sends no build abort when it is cancelled', () => {
        const { cmp, confirm } = setup();
        confirm.mockImplementation(() => EMPTY);
        const abort = vi.spyOn(TestBed.inject(EvaluationsService), 'abortBuild').mockReturnValue(of('Success'));
        inEvaluation(cmp, 'Building');
        open(cmp, target({ status: 'Building' })).get('Abort')!.command!();
        expect(abort).not.toHaveBeenCalled();
      });

      it('retries a build of a failed evaluation, then follows the reopened evaluation live', () => {
        const { cmp } = setup();
        const retry = vi.spyOn(TestBed.inject(EvaluationsService), 'retryBuild').mockReturnValue(of('Success'));
        const follow = vi.spyOn(cmp, 'startLiveUpdates').mockImplementation(() => {});
        inEvaluation(cmp, 'Failed');
        const reload = reloadedAs(cmp, 'Building');
        open(cmp, target({ status: 'Aborted' })).get('Retry')!.command!();
        expect(retry).toHaveBeenCalledWith('b1');
        expect(reload).toHaveBeenCalled();
        expect(cmp.evaluation()!.status).toBe('Building');
        expect(follow).toHaveBeenCalledWith('Building');
      });
    });
  });

  // #636: a live log arrives in batches rather than one line every tick.
  describe('following a live log', () => {
    type Follow = {
      pendingLogLines: string[];
      startLogDrainTimer: () => void;
      stopLogDrainTimer: () => void;
      appendStreamedLines: (lines: string[]) => void;
    };

    it('drains everything that arrived in one batch', () => {
      vi.useFakeTimers();
      const { cmp } = setup();
      const c = cmp as unknown as Follow;
      const append = vi.spyOn(c, 'appendStreamedLines');
      c.pendingLogLines.push(...Array.from({ length: 12 }, (_, i) => `l${i}`));
      c.startLogDrainTimer();
      vi.advanceTimersByTime(300);
      expect(append).toHaveBeenCalledTimes(1);
      expect(append.mock.calls[0][0]).toHaveLength(12);
      c.stopLogDrainTimer();
      vi.useRealTimers();
    });
  });

  describe('opening a finished log', () => {
    const chunkIndex = { total_chunks: 2, total_lines: 4200, total_bytes: 1, chunks: [] };

    it('hands the viewer the chunked log on the server instead of loading it', async () => {
      const { cmp } = setup();
      const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ message: chunkIndex }) });
      vi.stubGlobal('fetch', fetchMock);
      cmp.builds.set([build('b1', 'hash-hello.drv', 'Completed')]);
      cmp.logLoading.set(true);
      await (cmp as unknown as Internals).fetchInitialLogs('b1');
      expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(cmp.serverLog()?.total).toBe(4200);
      expect(cmp.logLineCount()).toBe(4200);
      expect(cmp.logLoading()).toBe(false);
    });

    it('keeps the #L line link in the URL for the first log only', () => {
      const { cmp } = setup();
      vi.stubGlobal('fetch', vi.fn(() => new Promise(() => undefined)));
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      cmp.selectBuild(build('b1', 'hash-hello.drv', 'Completed'), true);
      expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ preserveFragment: true }));
      cmp.logLineCount.set(12);
      cmp.selectBuild(build('b2', 'hash-world.drv', 'Completed'), true);
      expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ preserveFragment: false }));
    });
  });

  describe('dependency failed', () => {
    it('points to the failed dependency and opens its log', () => {
      const { cmp } = setup();
      vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false }));
      const service = TestBed.inject(EvaluationsService);
      vi.spyOn(service, 'getBuildGraph').mockReturnValue(of({
        root: 'd-app',
        nodes: [
          { id: 'd-app', build: 'b-app', name: 'app', path: '', status: 'DependencyFailed', created_at: '', updated_at: '' },
          { id: 'd-lib', build: 'b-lib', name: 'lib', path: '', status: 'FailedPermanent', created_at: '', updated_at: '' },
          { id: 'd-ok', build: 'b-ok', name: 'ok', path: '', status: 'Completed', created_at: '', updated_at: '' },
        ],
        edges: [],
      }));
      const app = build('b-app', 'app', 'DependencyFailed');
      const lib = build('b-lib', 'lib', 'FailedPermanent');
      cmp.builds.set([app, lib]);
      cmp.selectBuild(app, true);
      expect(cmp.failedDependencies().map((node) => node.name)).toEqual(['lib']);
      expect(cmp.buildOf(cmp.failedDependencies()[0])).toBe(lib);
      cmp.selectBuild(lib, true);
      expect(cmp.failedDependencies()).toEqual([]);
    });
  });

  // #636: with `?build=` the reader came for that log, so it loads before the list.
  describe('deep-linked build', () => {
    it('fetches the linked build before the build list', () => {
      const getBuild = vi.fn(() => of({
        id: 'b1', evaluation: 'eval-1', status: 'Completed', derivation_path: 'hash-hello.drv',
        architecture: 'x86_64-linux', worker: null, dispatched_job: null, output: {}, prioritized: true, ifd: true, created_at: '', updated_at: '',
      }));
      const getBuilds = vi.fn(() => of({ builds: [], total: 0, active_count: 0 }));
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap({ build: 'b1' }),
              fragment: null,
            },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00', started_at: null, finished_at: null, trigger: null }),
            getBuild,
            getBuilds,
            getEvaluationMessages: () => of([]),
          } },
        ],
      });
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}', { status: 404 }));
      fixture.componentInstance.ngOnInit();
      expect(getBuild).toHaveBeenCalled();
      expect(getBuild.mock.invocationCallOrder[0]).toBeLessThan(getBuilds.mock.invocationCallOrder[0]);
      expect(fixture.componentInstance.builds().find(b => b.id === 'b1')?.prioritized).toBe(true);
      expect(fixture.componentInstance.builds().find(b => b.id === 'b1')?.ifd).toBe(true);
      fixture.destroy();
    });

    function setupScoped(query: Record<string, string>, buildEvaluation: Observable<string> = of('eval-1'), listed: BuildItem[] = []) {
      const getBuild = vi.fn((id: string) => buildEvaluation.pipe(map((evaluation) => ({
        id, evaluation, status: 'Completed', derivation_path: `hash-${id}.drv`,
        architecture: 'x86_64-linux', worker: null, dispatched_job: null, output: {}, prioritized: false, ifd: false, created_at: '', updated_at: '',
      }))));
      const getBuilds = vi.fn((..._args: unknown[]) => of({ builds: listed, total: listed.length, active_count: 0 }));
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap(query),
              fragment: null,
            },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of({ id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00', started_at: null, finished_at: null, trigger: null }),
            getBuild,
            getBuilds,
            getEvaluationMessages: () => of([]),
          } },
        ],
      });
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}', { status: 404 }));
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      fixture.componentInstance.ngOnInit();
      return { cmp: fixture.componentInstance, fixture, getBuild, getBuilds, navigate };
    }

    it('selects the linked build without narrowing the list', () => {
      const { cmp, fixture, getBuilds } = setupScoped({ build: 'b1' });
      expect(getBuilds.mock.calls[0][3]).toBeUndefined();
      expect(cmp.scopeBuildId()).toBeNull();
      expect(cmp.selectedBuildId()).toBe('b1');
      fixture.destroy();
    });

    it('restores the picked dependency inside the package scope', () => {
      const { cmp, fixture, getBuild, getBuilds } = setupScoped({ package: 'pkg', build: 'dep' });
      expect(getBuild).toHaveBeenCalledWith('dep');
      expect(getBuilds.mock.calls[0][3]).toBe('pkg');
      expect(cmp.selectedBuildId()).toBe('dep');
      fixture.destroy();
    });

    it('keeps the package in the URL when a dependency is picked', () => {
      const { cmp, fixture, navigate } = setupScoped({ package: 'pkg' });
      expect(cmp.selectedBuildId()).toBe('pkg');
      const dep = { ...cmp.builds()[0], id: 'dep' };
      cmp.selectBuild(dep, true);
      expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({
        queryParams: { build: 'dep', eval: null },
        queryParamsHandling: 'merge',
      }));
      fixture.destroy();
    });

    it('names the package and lists every build after Show All Builds', () => {
      const { fixture, getBuilds, navigate } = setupScoped({ package: 'pkg' });
      fixture.detectChanges();
      const host = fixture.nativeElement as HTMLElement;
      expect(host.querySelector('.build-limit-label')?.textContent).toContain('Builds for pkg');
      host.querySelector<HTMLButtonElement>('.build-limit button')!.click();
      fixture.detectChanges();
      expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ queryParams: { package: null } }));
      expect(getBuilds.mock.lastCall?.[3]).toBeUndefined();
      expect(host.querySelector('.build-limit')).toBeNull();
      fixture.destroy();
    });

    it.each([
      ['of another evaluation', of('eval-2')],
      ['that does not exist', throwError(() => new ApiError('Build not found', 404))],
    ])('flags a linked build %s and opens the evaluation as usual', (_case, buildEvaluation) => {
      const failed = build('bad', 'hash-openssl.drv', 'FailedPermanent');
      const { cmp, fixture, navigate } = setupScoped({ build: 'other' }, buildEvaluation, [failed]);
      fixture.detectChanges();
      expect(cmp.missingBuildId()).toBe('other');
      expect(cmp.builds().map(b => b.id)).toEqual(['bad']);
      expect(cmp.selectedBuildId()).toBe('bad');
      expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { build: null } }));
      expect((fixture.nativeElement as HTMLElement).querySelector('.log-area gr-message-banner')?.textContent).toContain('does not belong');
      fixture.destroy();
    });
  });

  describe('opened without a build', () => {
    function setupEvaluationLogPageFixture(
      builds: BuildItem[],
      getBuilds: () => Observable<unknown> = () => of({ builds, total: builds.length, active_count: 0 }),
      evaluation: Partial<EvaluationResponse> = {},
    ) {
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap({}),
              fragment: null,
            },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of({ id: 'eval-1', status: 'Failed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00', started_at: null, finished_at: null, trigger: null, ...evaluation }),
            getBuilds,
            getEvaluationMessages: () => of([]),
          } },
        ],
      });
      vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}', { status: 404 }));
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      fixture.componentInstance.ngOnInit();
      return fixture;
    }

    it('shows a failed build list as an error with retry, never as "No builds"', () => {
      const getBuilds = vi.fn(() => throwError(() => new ApiError('database is down', 500)));
      const fixture = setupEvaluationLogPageFixture([], getBuilds);
      fixture.detectChanges();
      const root = fixture.nativeElement as HTMLElement;
      expect(root.querySelector('.no-builds')).toBeNull();
      expect(root.querySelector('.log-sidebar')?.textContent).toContain('Failed to load builds');
      expect(root.querySelector('.log-sidebar')?.textContent).toContain('database is down');
      getBuilds.mockReturnValue(of({ builds: [build('ok', 'hash-zlib.drv')], total: 1, active_count: 0 }) as never);
      (Array.from(root.querySelectorAll('.log-sidebar button')) as HTMLButtonElement[])
        .find((b) => b.textContent?.includes('Try Again'))?.click();
      fixture.detectChanges();
      expect(root.querySelector('.log-sidebar')?.textContent).toContain('zlib');
      fixture.destroy();
    });

    it('shows a failed log request as an error with retry, not as an empty log', async () => {
      const fixture = setupEvaluationLogPageFixture([build('bad', 'hash-openssl.drv', 'Building')]);
      vi.mocked(globalThis.fetch).mockResolvedValue(new Response('{"message":"log store is down"}', { status: 500 }));
      fixture.componentInstance.retryLog();
      await new Promise((resolve) => setTimeout(resolve));
      fixture.detectChanges();
      const root = fixture.nativeElement as HTMLElement;
      expect(fixture.componentInstance.logRequestError()).toBeInstanceOf(ApiError);
      expect(root.querySelector('.log-area')?.textContent).toContain('Failed to load log');
      expect(root.querySelector('.log-area')?.textContent).toContain('log store is down');
      expect(root.querySelector('gr-log-viewer')).toBeNull();
      fixture.destroy();
    });

    it('says the build list is partial and loads the next page on request', () => {
      const getBuilds = vi.fn((..._args: unknown[]) => of({ builds: [build('ok', 'hash-zlib.drv')], total: 3, active_count: 0 }));
      const fixture = setupEvaluationLogPageFixture([], getBuilds as never);
      fixture.detectChanges();
      const root = fixture.nativeElement as HTMLElement;
      expect(root.querySelector('.sidebar-builds-heading')?.textContent?.trim()).toBe('1 of 3 builds');
      getBuilds.mockReturnValue(of({ builds: [build('a', 'hash-a.drv'), build('b', 'hash-b.drv')], total: 3, active_count: 0 }));
      (root.querySelector('gr-load-more button') as HTMLButtonElement).click();
      fixture.detectChanges();
      expect(getBuilds.mock.lastCall?.[2]).toBe(1);
      expect(root.querySelector('.sidebar-builds-heading')?.textContent?.trim()).toBe('Builds');
      expect(root.querySelector('gr-load-more button')).toBeNull();
      fixture.destroy();
    });

    it('Open the first failed build when no build is in progress.', () => {
      const fixture = setupEvaluationLogPageFixture([
        build('ok', 'hash-zlib.drv'), build('dep', 'hash-app.drv', 'DependencyFailed'), build('bad', 'hash-openssl.drv', 'FailedPermanent', 1),
      ]);
      expect(fixture.componentInstance.selectedBuildId()).toBe('bad');
      fixture.destroy();
    });

    it('Link from the info bar to the evaluation summary', () => {
      const fixture = setupEvaluationLogPageFixture([]);
      fixture.detectChanges();
      const link = (fixture.nativeElement as HTMLElement).querySelector('.info-bar-right a[href]');
      expect(link?.getAttribute('href')).toBe('/project/proj/summary/eval-1');
      fixture.destroy();
    });

    it('leaves the duration out until the evaluation starts', () => {
      const waiting = setupEvaluationLogPageFixture([], undefined, { status: 'Waiting' });
      waiting.detectChanges();
      expect((waiting.nativeElement as HTMLElement).querySelector('.info-bar')?.textContent).not.toContain('Duration');
      waiting.destroy();
      TestBed.resetTestingModule();
      const started = setupEvaluationLogPageFixture([], undefined, { status: 'Completed', started_at: '2026-01-01T00:00:00', finished_at: '2026-01-01T00:00:30' });
      started.detectChanges();
      expect((started.nativeElement as HTMLElement).querySelector('.info-bar')?.textContent).toContain('Duration');
      started.destroy();
    });

    it('explains a Building evaluation whose builds are done by the waiting exec job', () => {
      const fixture = setupEvaluationLogPageFixture([build('ok', 'hash-zlib.drv')], undefined, { status: 'Building' });
      fixture.componentInstance.execJobWaitingWithReason.set({
        id: 'j1', entry_point: 'apps.x86_64-linux.train', status: 'created', waiting_reason: { kind: 'manual_start' },
        nodes: 1, labels: [], devices: [], autostart: false, keep_vm_running: false,
      });
      fixture.detectChanges();
      expect((fixture.nativeElement as HTMLElement).querySelector('.log-area gr-waiting-notice')?.textContent).toContain('Waiting for manual start');
      fixture.destroy();
    });

    it.each([
      ['Aborted', 'Aborted before any builds started'],
      ['Completed', 'No builds'],
    ] as const)('shows one main message and no sidebar list for a %s evaluation without builds', (status, message) => {
      const fixture = setupEvaluationLogPageFixture([], undefined, { status });
      fixture.detectChanges();
      const root = fixture.nativeElement as HTMLElement;
      expect(root.querySelector('.no-builds')).toBeNull();
      expect(root.querySelector('.builds-list')).toBeNull();
      expect(root.querySelector('.log-area .hint-text')?.textContent?.trim()).toBe(message);
      fixture.destroy();
    });
  });

  describe('evaluation progress', () => {
    const rows = (name: string) => [{ name, state: 'Fetching' as const, downloaded_bytes: 1, expected_bytes: 0 }];

    function setupLive(progress: EvaluationResponse['progress'], status = 'Fetching', builds: BuildItem[] = []) {
      const frames = new Subject<LiveEvent>();
      const getEvaluation = vi.fn(() => of({
        id: 'eval-1', status, created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
        started_at: '2026-01-01T00:00:00', finished_at: null, trigger: null, progress,
      }));
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap({}),
              fragment: null,
            },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation,
            getBuilds: () => of({ builds, total: builds.length, active_count: 0 }),
            getEvaluationMessages: () => of([]),
          } },
          { provide: LiveService, useValue: { connect: () => frames } },
        ],
      });
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      fixture.componentInstance.ngOnInit();
      return { fixture, frames, getEvaluation };
    }

    it('drops the live badge and the socket once the evaluation reaches a final status', async () => {
      const { fixture, frames, getEvaluation } = setupLive(null, 'Building');
      const cmp = fixture.componentInstance;
      fixture.detectChanges();
      cmp.liveConnectionOpen.set(true);
      fixture.detectChanges();
      expect(fixture.nativeElement.querySelector('gr-live-status')).not.toBeNull();
      getEvaluation.mockImplementation(() => of({
        id: 'eval-1', status: 'Failed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
        started_at: '2026-01-01T00:00:00', finished_at: null, trigger: null, progress: null,
      }));
      frames.next({ event: 'evaluation.status', at: '', content: {} } as LiveEvent);
      await new Promise((resolve) => setTimeout(resolve, 350));
      fixture.detectChanges();
      expect(cmp.evaluation()?.status).toBe('Failed');
      expect(fixture.nativeElement.querySelector('gr-live-status')).toBeNull();
      expect(frames.observed).toBe(false);
      fixture.destroy();
    });

    it('applies activity frames without refetching the evaluation', () => {
      const { fixture, frames, getEvaluation } = setupLive({ kind: 'fetching', inputs: [] });
      expect(fixture.componentInstance.progress()).toEqual({ kind: 'fetching', inputs: [] });
      getEvaluation.mockClear();

      frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'eval-1', progress: { kind: 'fetching', inputs: rows('nixpkgs') } } });
      expect(fixture.componentInstance.progress()).toEqual({ kind: 'fetching', inputs: rows('nixpkgs') });
      expect(getEvaluation).not.toHaveBeenCalled();
      fixture.detectChanges();
      expect(fixture.nativeElement.querySelector('gr-input-fetch-list .input-row')?.textContent).toContain('nixpkgs');
      fixture.destroy();
    });

    it('keeps the thunk bar while builds appear during the evaluation', () => {
      const thunks = { kind: 'evaluating' as const, thunks: 1234567 };
      const { fixture } = setupLive(thunks, 'EvaluatingDerivation', [build('b1', 'hello.drv', 'Queued')]);
      fixture.detectChanges();
      expect(fixture.nativeElement.querySelector('.evaluating-hint gr-thunk-progress gr-segmented-bar')).not.toBeNull();
      expect(fixture.nativeElement.textContent).not.toContain('Discovering builds');
      fixture.destroy();
    });

    it('keeps a live frame over an older snapshot from a refetch', async () => {
      const { fixture, frames, getEvaluation } = setupLive({ kind: 'fetching', inputs: rows('stale') });
      frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'eval-1', progress: { kind: 'fetching', inputs: rows('live') } } });
      getEvaluation.mockClear();

      frames.next({ event: 'build.status', at: '', content: {} } as LiveEvent);
      await new Promise(resolve => setTimeout(resolve, 350));
      expect(getEvaluation).toHaveBeenCalled();
      expect(fixture.componentInstance.progress()).toEqual({ kind: 'fetching', inputs: rows('live') });
      fixture.destroy();
    });

    it('keeps following the builds after an abort until no build is active', async () => {
      const frames = new Subject<LiveEvent>();
      const state = { evaluation: 'Building', build: 'Building' as BuildItem['status'] };
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap({}),
              fragment: null,
            },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of({
              id: 'eval-1', status: state.evaluation, created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
              started_at: '2026-01-01T00:00:00', finished_at: null, trigger: null,
            }),
            getBuild: () => of({ progress: null }),
            getBuilds: () => of({
              builds: [build('b1', 'hash-hello.drv', state.build)], total: 1, active_count: state.build === 'Building' ? 1 : 0,
            }),
            getEvaluationMessages: () => of([]),
            abortEvaluation: () => of('ok'),
          } },
          { provide: LiveService, useValue: { connect: () => frames } },
        ],
      });
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}', { status: 404 }));
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      const cmp = fixture.componentInstance;
      cmp.ngOnInit();

      state.evaluation = 'Aborted';
      cmp.requestAbort(new MouseEvent('click', { ctrlKey: true }));
      expect(cmp.evaluation()?.status).toBe('Aborted');
      expect(cmp.builds()[0].status).toBe('Building');
      expect(frames.observed).toBe(true);

      state.build = 'Aborted';
      frames.next({ event: 'build.status', at: '', content: {} } as LiveEvent);
      await new Promise(resolve => setTimeout(resolve, 350));
      expect(cmp.builds()[0].status).toBe('Aborted');
      expect(frames.observed).toBe(false);
      fixture.destroy();
    });
  });

  describe('build progress', () => {
    type Progress = { phase: 'prefetch' | 'download' | 'upload'; bytes_done: number; bytes_total: number | null; paths_done: number; paths_total: number | null };
    const MiB = 1024 * 1024;
    const frame = (p: Progress): LiveEvent => ({ event: 'build.progress', at: '', content: { derivation_build: 'd1', ...p } });

    function setupBuild(seed: Progress | null) {
      const frames = new Subject<LiveEvent>();
      const getBuild = vi.fn(() => of({ progress: seed }));
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap({}),
              fragment: null,
            },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of({ id: 'eval-1', status: 'Building', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00', started_at: '2026-01-01T00:00:00', finished_at: null, trigger: null }),
            getBuild,
            getBuilds: () => of({ builds: [build('b1', 'hash-hello.drv', 'Building')], total: 1, active_count: 1 }),
            getEvaluationMessages: () => of([]),
          } },
          { provide: LiveService, useValue: { connect: () => frames } },
        ],
      });
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}', { status: 404 }));
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      const cmp = fixture.componentInstance;
      cmp.ngOnInit();
      cmp.selectedBuildId.set('b1');
      cmp.logLoading.set(false);
      (cmp as unknown as { watchProgress: (id: string) => void }).watchProgress('b1');
      fixture.detectChanges();
      const panel = () => fixture.nativeElement.querySelector('gr-build-progress') as HTMLElement | null;
      return { fixture, cmp, frames, getBuild, panel };
    }

    it('shows a prefetch as a compact strip above the running log', () => {
      const { fixture, panel } = setupBuild({ phase: 'prefetch', bytes_done: 120 * MiB, bytes_total: 340 * MiB, paths_done: 12, paths_total: 40 });
      expect(panel()?.querySelector('.build-progress--compact')).not.toBeNull();
      expect(panel()?.closest('gr-log-viewer')).not.toBeNull();
      expect(panel()?.textContent).toContain('Prefetching inputs 35 %');
      expect(panel()?.textContent).toContain('120 / 340 MiB');
      expect(panel()?.textContent).toContain('12 / 40 paths');
      fixture.destroy();
    });

    it('shows a download in place of the log', () => {
      const { fixture, panel } = setupBuild({ phase: 'download', bytes_done: 1536, bytes_total: null, paths_done: 0, paths_total: 1 });
      expect(panel()?.querySelector('.build-progress--compact')).toBeNull();
      expect(fixture.nativeElement.querySelector('gr-log-viewer')).toBeNull();
      expect(panel()?.textContent).toContain('Downloading');
      expect(panel()?.textContent).toContain('1.5 KiB');
      fixture.destroy();
    });

    it('hands the panel back to the log once the download landed', () => {
      const { fixture, panel } = setupBuild({ phase: 'download', bytes_done: 1536, bytes_total: 1536, paths_done: 1, paths_total: 1 });
      expect(panel()).toBeNull();
      expect(fixture.nativeElement.querySelector('gr-log-viewer')).not.toBeNull();
      fixture.destroy();
    });

    it('follows live frames into the upload without refetching the build, and drops a finished phase', () => {
      const { fixture, frames, getBuild, panel } = setupBuild(null);
      expect(panel()).toBeNull();

      frames.next(frame({ phase: 'upload', bytes_done: 30, bytes_total: 120, paths_done: 1, paths_total: 3 }));
      fixture.detectChanges();
      expect(panel()?.textContent).toContain('Uploading outputs 25 %');
      expect(panel()?.textContent).toContain('1 / 3 paths');

      frames.next(frame({ phase: 'upload', bytes_done: 120, bytes_total: 120, paths_done: 3, paths_total: 3 }));
      fixture.detectChanges();
      expect(panel()).toBeNull();
      expect(getBuild).toHaveBeenCalledTimes(1);
      fixture.destroy();
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

    it('saves the whole log as plain text under the store file name', async () => {
      const { cmp } = setup();
      const getLog = vi.spyOn(TestBed.inject(EvaluationsService), 'getBuildLog')
        .mockReturnValue(of('\u001b[31mline 1\u001b[0m\nline 2\n'));
      const created: Blob[] = [];
      objectUrls['createObjectURL'] = (b: Blob) => (created.push(b), 'blob:log');
      const revoke = vi.fn();
      objectUrls['revokeObjectURL'] = revoke;
      const clicked: HTMLAnchorElement[] = [];
      vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
        clicked.push(this);
      });

      await cmp.downloadLog(build('b1', 'hash-hello-1.0.drv'));

      expect(getLog).toHaveBeenCalledWith('b1');
      expect(await created[0].text()).toBe('line 1\nline 2\n');
      expect(clicked[0].download).toBe('hash-hello-1.0.log');
      expect(revoke).toHaveBeenCalledWith('blob:log');
    });

    it('reports the server reason and saves nothing when the log cannot be read', async () => {
      const { cmp } = setup();
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      const error = new ApiError('Log not found', 404);
      vi.spyOn(TestBed.inject(EvaluationsService), 'getBuildLog').mockReturnValue(throwError(() => error));
      const create = vi.fn();
      objectUrls['createObjectURL'] = create;

      await cmp.downloadLog(build('b1', 'hash-hello-1.0.drv'));

      expect(create).not.toHaveBeenCalled();
      expect(failure).toHaveBeenCalledWith("Couldn't download log", error);
    });

    it('never saves an empty file, telling why a build that never ran has no log', async () => {
      const { cmp } = setup();
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      vi.spyOn(TestBed.inject(EvaluationsService), 'getBuildLog').mockReturnValue(of(''));
      const create = vi.fn();
      objectUrls['createObjectURL'] = create;

      await cmp.downloadLog(build('b1', 'hash-hello-1.0.drv', 'DependencyFailed'));

      expect(create).not.toHaveBeenCalled();
      expect(failure.mock.calls[0][0]).toContain('dependencies failed');
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

    it('asks first on a plain click of Abort, naming the task and commit, then aborts', () => {
      const { cmp, confirm } = setup();
      cmp.evaluationId = 'eval-1';
      cmp.evaluation.set({ id: 'eval-1', status: 'Building', commit: 'abc1234def5678', task_name: 'ci', task_display_name: 'CI' } as EvaluationResponse);
      const abort = vi.spyOn(TestBed.inject(EvaluationsService), 'abortEvaluation').mockReturnValue(of('ok'));
      const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
      cmp.requestAbort(new MouseEvent('click'));
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Abort Evaluation', target: 'CI · abc1234d' }));
      expect(abort).toHaveBeenCalledWith('eval-1');
      expect(success).toHaveBeenCalledWith('Evaluation aborted');
    });

    it('sends nothing when the abort is cancelled', () => {
      const { cmp, confirm } = setup();
      confirm.mockImplementation(() => EMPTY);
      const abort = vi.spyOn(TestBed.inject(EvaluationsService), 'abortEvaluation').mockReturnValue(of('ok'));
      cmp.requestAbort(new MouseEvent('click'));
      expect(confirm).toHaveBeenCalledOnce();
      expect(abort).not.toHaveBeenCalled();
    });

    it('aborts at once on a Ctrl click, with no confirmation', () => {
      const { cmp, confirm } = setup();
      const abort = vi.spyOn(TestBed.inject(EvaluationsService), 'abortEvaluation').mockReturnValue(of('ok'));
      cmp.requestAbort(new MouseEvent('click', { ctrlKey: true }));
      expect(abort).toHaveBeenCalledOnce();
      expect(confirm).not.toHaveBeenCalled();
      expect(cmp.aborting()).toBe(false);
    });

    it('reports a refused Ctrl-click abort as a toast, since no dialog is open', () => {
      const { cmp } = setup();
      const refused = new ApiError('Evaluation already finished', 409);
      vi.spyOn(TestBed.inject(EvaluationsService), 'abortEvaluation').mockReturnValue(throwError(() => refused));
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      cmp.requestAbort(new MouseEvent('click', { ctrlKey: true }));
      expect(failure).toHaveBeenCalledWith('Failed to abort evaluation', refused);
      expect(cmp.aborting()).toBe(false);
    });
  });

  describe('build write failures', () => {
    it('reports a failed sbom download with the server reason', () => {
      const { cmp } = setup();
      const missing = new ApiError('Build has no SBOM', 404);
      vi.spyOn(TestBed.inject(EvaluationsService), 'downloadBuildSbom').mockReturnValue(throwError(() => missing));
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      cmp.downloadSbom(build('b1', 'hello'));
      expect(failure).toHaveBeenCalledWith('Failed to download SBOM', missing);
    });

    it('reports a failed retry with the server reason', () => {
      const { cmp } = setup();
      const refused = new ApiError('Build is still running', 409);
      vi.spyOn(TestBed.inject(EvaluationsService), 'retryBuild').mockReturnValue(throwError(() => refused));
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      cmp.retryBuild(build('b1', 'hello', 'Failed'));
      expect(failure).toHaveBeenCalledWith('Failed to retry build', refused);
    });
  });

  describe('sidebar breadcrumb', () => {
    it('leads from the project to the task by display name, back at this evaluation', () => {
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: {
              paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }),
              queryParamMap: convertToParamMap({}),
              fragment: null,
            },
          } },
          { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'MyProject' }) } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of({
              id: 'eval-1', status: 'Completed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
              started_at: null, finished_at: null, trigger: null, task_name: 'nightly', task_display_name: 'Nightly',
            }),
            getBuilds: () => of({ builds: [], total: 0, active_count: 0 }),
            getEvaluationMessages: () => of([]),
          } },
        ],
      });
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      fixture.detectChanges();

      const crumbs = Array.from(
        (fixture.nativeElement as HTMLElement).querySelectorAll('nav[aria-label="Breadcrumb"] .breadcrumb-link, nav[aria-label="Breadcrumb"] .breadcrumb-current'),
      );
      expect(crumbs.map((crumb) => crumb.textContent?.trim())).toEqual(['Projects', 'MyProject', 'Nightly']);
      expect(crumbs.map((crumb) => crumb.getAttribute('href'))).toEqual([
        '/projects',
        '/project/proj',
        '/project/proj/task/nightly?eval=eval-1',
      ]);
      fixture.destroy();
    });
  });

  describe('load failures', () => {
    const EVALUATION = {
      id: 'eval-1', status: 'Failed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
      started_at: null, finished_at: null, trigger: null,
    };

    function render(service: Record<string, unknown>) {
      TestBed.configureTestingModule({
        imports: [EvaluationLogComponent],
        providers: [
          provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {
            snapshot: { paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }), queryParamMap: convertToParamMap({}), fragment: null },
          } },
          { provide: EvaluationsService, useValue: {
            getEvaluation: () => of(EVALUATION),
            getBuilds: () => of({ builds: [], total: 0, active_count: 0 }),
            getEvaluationMessages: () => of([]),
            ...service,
          } },
        ],
      });
      const fixture = TestBed.createComponent(EvaluationLogComponent);
      fixture.detectChanges();
      return { fixture, root: fixture.nativeElement as HTMLElement };
    }

    it('says a server error is a failed load, not a deleted evaluation, and loads again on Try Again', () => {
      const getEvaluation = vi.fn()
        .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
        .mockReturnValue(of(EVALUATION));
      const { fixture, root } = render({ getEvaluation });
      expect(root.textContent).toContain('Failed to load evaluation');
      expect(root.textContent).toContain('database is down');
      expect(root.textContent).not.toContain('Evaluation not found');

      root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
      fixture.detectChanges();
      expect(getEvaluation).toHaveBeenCalledTimes(2);
      expect(fixture.componentInstance.evaluation()?.id).toBe('eval-1');
      fixture.destroy();
    });

    it('keeps the not-found wording for a 404', () => {
      const { fixture, root } = render({ getEvaluation: () => throwError(() => new ApiError('Evaluation not found', 404)) });
      expect(root.textContent).toContain('Evaluation not found');
      expect(root.textContent).not.toContain('Failed to load evaluation');
      fixture.destroy();
    });

    it('lists the exec jobs of an exec task in the sidebar of the default view, linked to their page', () => {
      TestBed.overrideProvider(ExecJobsService, { useValue: {
        forEvaluation: () => of([{ id: 'job-1', entry_point: 'deploy', status: 'created', waiting_reason: { kind: 'manual_start' }, nodes: 1, labels: [], devices: [] }]),
      } });
      const { fixture, root } = render({ getEvaluation: () => of({ ...EVALUATION, task_name: 'exec' }) });
      const sidebarJobs = root.querySelector('.log-sidebar app-exec-job-list');
      expect(sidebarJobs?.textContent).toContain('deploy');
      expect(sidebarJobs?.textContent).toContain('waits for a manual start');
      expect(sidebarJobs?.querySelector('a.sidebar-row-link')?.getAttribute('href')).toContain('/exec/job-1');
      fixture.componentInstance.selectEvaluationSection();
      fixture.detectChanges();
      expect(root.querySelectorAll('app-exec-job-list').length).toBe(1);
      fixture.destroy();
    });

    it('shows a failed message load instead of "No messages"', () => {
      const { fixture, root } = render({ getEvaluationMessages: () => throwError(() => new ApiError('messages unavailable', 500)) });
      fixture.componentInstance.selectEvaluationSection();
      fixture.detectChanges();
      expect(root.textContent).toContain('Failed to load evaluation messages');
      expect(root.textContent).not.toContain('No messages');
      fixture.destroy();
    });
  });

  it('shows the evaluation and task the route already fetched without fetching them again', () => {
    const evaluation = {
      id: 'eval-1', status: 'Failed', created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
      started_at: null, finished_at: null, trigger: null, task: 't1', task_name: 'nightly',
    };
    const getEvaluation = vi.fn(() => of(evaluation));
    const getTask = vi.fn(() => of({ id: 't1', permissions: ['abortEvaluation'] }));
    TestBed.configureTestingModule({
      imports: [EvaluationLogComponent],
      providers: [
        provideRouter([]), provideHttpClient(), provideHttpClientTesting(),
        { provide: ActivatedRoute, useValue: {
          snapshot: {
            paramMap: convertToParamMap({ project: 'proj', evaluationId: 'eval-1' }), queryParamMap: convertToParamMap({}), fragment: null,
            data: { evaluationAccess: { evaluation, task: { id: 't1', permissions: ['abortEvaluation'], managed: true } } },
          },
        } },
        { provide: EvaluationsService, useValue: {
          getEvaluation,
          getBuilds: () => of({ builds: [], total: 0, active_count: 0 }),
          getEvaluationMessages: () => of([]),
        } },
        { provide: TasksService, useValue: { getTask, getTaskInfo: () => NEVER } },
      ],
    });
    const fixture = TestBed.createComponent(EvaluationLogComponent);
    fixture.detectChanges();
    expect(fixture.componentInstance.evaluation()?.id).toBe('eval-1');
    expect(fixture.componentInstance.access().managed).toBe(true);
    expect(getEvaluation).not.toHaveBeenCalled();
    expect(getTask).not.toHaveBeenCalled();
    expect((fixture.nativeElement as HTMLElement).querySelectorAll('gr-eval-status-badge')).toHaveLength(1);
    fixture.destroy();
  });
});
