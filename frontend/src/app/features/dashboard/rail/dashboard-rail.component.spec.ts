/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { By } from '@angular/platform-browser';
import { provideRouter } from '@angular/router';
import { NEVER, Observable, Subject, of, tap, throwError } from 'rxjs';
import { DashboardRailComponent } from './dashboard-rail.component';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
import { Rail, RailCache, RailProject, StarTarget } from '@core/models';
import { TooltipDirective } from '@gradient/ui/ui';

function mount(rail: () => Observable<Rail>) {
  const changed = new Subject<StarTarget>();
  TestBed.configureTestingModule({
    imports: [DashboardRailComponent],
    providers: [
      provideRouter([]),
      { provide: DashboardService, useValue: { rail: vi.fn(rail) } },
      { provide: StarsService, useValue: {
        changed$: changed,
        set: (target: StarTarget) => of(true).pipe(tap(() => changed.next(target))),
      } },
    ],
  });
  const f = TestBed.createComponent(DashboardRailComponent);
  const empty: boolean[] = [];
  f.componentInstance.empty.subscribe((e) => empty.push(e));
  f.detectChanges();
  return { f, root: f.nativeElement as HTMLElement, empty };
}

const createRailProjectFixture = (name: string, extra: Partial<RailProject> = {}): RailProject =>
  ({ name, display_name: name.toUpperCase(), starred: false, tier: 'active', status: 'Completed', task_count: 1, ...extra });
const createRailCacheFixture = (name: string, extra: Partial<RailCache> = {}): RailCache =>
  ({ name, display_name: name.toUpperCase(), starred: false, nar_count: 1, ...extra });

const hrefs = (root: HTMLElement) => Array.from(root.querySelectorAll('a[href]')).map((a) => a.getAttribute('href'));
const getTooltipTextsBySelector = (f: ComponentFixture<DashboardRailComponent>, selector: string) =>
  f.debugElement.queryAll(By.css(selector)).map((el) => el.injector.get(TooltipDirective).grTooltip());
const getVisibleCardHeadingTitlesWithoutChevronIconText = (root: HTMLElement) =>
  Array.from(root.querySelectorAll('h2')).map((h) => h.textContent!.replace('chevron_right', '').trim());
const getVisibleTopLevelProjectRowNames = (root: HTMLElement) =>
  Array.from(root.querySelectorAll('.rail-card:first-child > ul > li > .row .n')).map((n) => n.textContent!.trim());

describe('DashboardRailComponent', () => {
  it('links the card headings, projects, nested tasks and caches directly', () => {
    const { f, root } = mount(() => of({
      projects: [createRailProjectFixture('infra', { display_name: 'Infra', starred: true, tier: 'starred_active', status: 'Failed', task_count: 2,
        tasks: [{ name: 'hosts', display_name: 'Hosts', status: 'Failed' }] })],
      caches: [createRailCacheFixture('main', { display_name: 'Main', nar_count: 1200 })],
    }));
    expect(getVisibleCardHeadingTitlesWithoutChevronIconText(root)).toEqual(['Projects', 'Caches']);
    expect(Array.from(root.querySelectorAll('h2 a.card-heading-link')).map((a) => a.getAttribute('href'))).toEqual(['/projects', '/caches']);
    expect(hrefs(root)).toContain('/project/infra');
    expect(hrefs(root)).toContain('/project/infra/task/hosts');
    expect(root.querySelector('.tasks .n')?.textContent?.trim()).toBe('Hosts');
    expect(hrefs(root)).toContain('/caches/main');
    expect(root.textContent).not.toContain('All projects');
    expect(root.textContent).not.toContain('All caches');
    expect(getTooltipTextsBySelector(f, '.n')).toEqual(['Infra (infra) · 2 tasks', 'Hosts (hosts)', 'Main (main)']);
  });

  it('names the status of each dot in its tooltip instead of counting tasks in text', () => {
    const { f, root } = mount(() => of({
      projects: [
        createRailProjectFixture('a', { starred: true, status: 'EvaluatingFlake', tasks: [{ name: 't', display_name: 'T', status: 'Failed' }] }),
        createRailProjectFixture('b', { status: null, task_count: 3 }),
      ],
      caches: [],
    }));
    expect(getTooltipTextsBySelector(f, '.dot')).toEqual(['Evaluating', 'Failed', 'No evaluations yet']);
    expect(root.textContent).not.toContain('3 tasks');
  });

  it('shows starred items first, cuts each card at five rows and counts the total only when cut', () => {
    const { root } = mount(() => of({
      projects: ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map((name) => createRailProjectFixture(name, { starred: name === 'f' })),
      caches: ['x', 'y'].map((name) => createRailCacheFixture(name, { starred: name === 'y' })),
    }));
    expect(getVisibleTopLevelProjectRowNames(root)).toEqual(['F', 'A', 'B', 'C', 'D']);
    expect(Array.from(root.querySelectorAll('.rail-card:last-child .row .n')).map((n) => n.textContent!.trim())).toEqual(['Y', 'X']);
    expect(Array.from(root.querySelectorAll('.card-heading-link gr-badge')).map((b) => b.textContent!.trim())).toEqual(['7']);
  });

  it('shows the short package count with the full count in a tooltip', () => {
    const { f, root } = mount(() => of({ projects: [createRailProjectFixture('a')], caches: [createRailCacheFixture('main', { nar_count: 41210 })] }));
    expect(root.querySelector('.rail-card:last-child .count')?.textContent?.trim()).toBe('41.2k');
    expect(getTooltipTextsBySelector(f, '.count')).toEqual(['41,210 packages']);
    expect(root.querySelector('a[href="/caches/main"]')?.getAttribute('aria-label')).toBe('MAIN (main) · 41,210 packages');
    expect(root.querySelector('a[href="/project/a"]')?.getAttribute('aria-label')).toBe('A (a) · 1 task');
  });

  it('hides a card with nothing in it', () => {
    const { root } = mount(() => of({ projects: [createRailProjectFixture('a')], caches: [] }));
    expect(getVisibleCardHeadingTitlesWithoutChevronIconText(root)).toEqual(['Projects']);
  });

  it('keeps the star of an unstarred row hidden until the row is hovered or focused', () => {
    const { root } = mount(() => of({ projects: [createRailProjectFixture('a', { starred: true }), createRailProjectFixture('b')], caches: [] }));
    expect(Array.from(root.querySelectorAll('gr-star-button')).map((s) => s.classList.contains('star-button-shown-on-hover'))).toEqual([false, true]);
    expect(root.querySelectorAll('gr-star-button button')[1].getAttribute('aria-label')).toBe('Star B');
  });

  it('loads again after a star, which moves the project into its new place', () => {
    const before: Rail = {
      projects: [createRailProjectFixture('a', { tier: 'member' }), createRailProjectFixture('b', { tier: 'member' })],
      caches: [],
    };
    const after: Rail = { ...before, projects: [{ ...before.projects[1], starred: true, tier: 'starred' }, before.projects[0]] };
    let calls = 0;
    const { f, root } = mount(() => of(++calls === 1 ? before : after));

    (root.querySelectorAll('gr-star-button button')[1] as HTMLElement).click();
    f.detectChanges();

    expect(getVisibleTopLevelProjectRowNames(root)).toEqual(['B', 'A']);
  });

  it('holds both cards with placeholder rows until the rail arrives', () => {
    const { root, empty } = mount(() => NEVER);
    expect(getVisibleCardHeadingTitlesWithoutChevronIconText(root)).toEqual(['Projects', 'Caches']);
    expect(root.querySelectorAll('gr-skeleton').length).toBeGreaterThan(0);
    expect(root.querySelector('.rail')?.getAttribute('aria-busy')).toBe('true');
    expect(empty).toEqual([]);
  });

  it('reports an empty rail', () => {
    expect(mount(() => of({ projects: [], caches: [] })).empty).toEqual([true]);
  });

  it('keeps onboarding for a user who only sees caches', () => {
    const rail: Rail = { projects: [], caches: [createRailCacheFixture('main')] };
    expect(mount(() => of(rail)).empty).toEqual([true]);
  });

  it('leaves a failed load to the page banner and shows the rail after a new load', () => {
    let calls = 0;
    const rail: Rail = { projects: [], caches: [createRailCacheFixture('main')] };
    const { f, root, empty } = mount(() => (++calls === 1 ? throwError(() => ({ status: 500 })) : of(rail)));
    expect(empty).toEqual([false]);
    expect(f.componentInstance.failed()).toBe(true);
    expect(root.textContent!.trim()).toBe('');
    f.componentInstance.load();
    f.detectChanges();
    expect(hrefs(root)).toContain('/caches/main');
  });

  it('hides itself on 403', () => {
    const { root, empty } = mount(() => throwError(() => ({ status: 403 })));
    expect(root.textContent!.trim()).toBe('');
    expect(empty).toEqual([false]);
  });
});
