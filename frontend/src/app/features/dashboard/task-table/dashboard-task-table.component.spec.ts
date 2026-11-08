/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { BehaviorSubject, NEVER, Observable, Subject, of, throwError } from 'rxjs';
import { DashboardTaskTableComponent } from './dashboard-task-table.component';
import { DashboardService } from '@core/services/dashboard.service';
import { TaskRow, TasksPage } from '@core/models';

const row = (task: string): TaskRow => ({
  project: 'infra',
  task,
  starred: false,
  tier: 'active',
  latest: { id: 'e1', status: 'Failed', commit: 'a1b2c3d4', created_at: '2026-09-23T10:00:00' },
  entry_points: { ok: 41, failing: 4, total: 45 },
  delta: 3,
  speed_ms: 840_000,
  history: [],
});

const PAGE: TasksPage = {
  filter: 'all',
  counts: { all: 12, failing: 3, starred: 0 },
  total: 12,
  tasks: [row('hosts')],
};
const STARRED: TasksPage = { ...PAGE, filter: 'starred', counts: { all: 12, failing: 3, starred: 2 } };
const chipLabels = (root: HTMLElement) =>
  Array.from(root.querySelectorAll('gr-tab-switch button')).map((c) => c.textContent?.replace(/\s+/g, ' ').trim());

function render(filter: string | null, page: () => Observable<TasksPage>, extraQueryParams: Record<string, string> = {}) {
  const tasks = vi.fn(page);
  const params = new BehaviorSubject(convertToParamMap({ ...(filter ? { filter } : {}), ...extraQueryParams }));
  TestBed.configureTestingModule({
    imports: [DashboardTaskTableComponent],
    providers: [
      provideRouter([]),
      { provide: DashboardService, useValue: { tasks } },
      { provide: ActivatedRoute, useValue: { queryParamMap: params } },
    ],
  });
  const f = TestBed.createComponent(DashboardTaskTableComponent);
  f.detectChanges();
  return { f, tasks, params, root: f.nativeElement as HTMLElement };
}

async function settle(f: ComponentFixture<unknown>) {
  f.detectChanges();
  await f.whenStable();
  f.detectChanges();
}

describe('DashboardTaskTableComponent', () => {
  afterEach(() => vi.restoreAllMocks());

  it('reads the chip from the url and shows every count', async () => {
    const { f, tasks, root } = render('failing', () => of(PAGE));
    await settle(f);
    expect(tasks).toHaveBeenCalledWith('failing', 1, 10, 60);
    expect(chipLabels(root)).toEqual(['All 12', 'Failing 3']);
    expect(root.querySelector('gr-tab-switch button.is-selected')?.textContent).toContain('Failing');
  });

  it('Make the waiting reason visible for the latest evaluation of a waiting task.', async () => {
    const waiting = row('hosts');
    waiting.latest = { ...waiting.latest!, status: 'Waiting', waiting_reason: { kind: 'no_cache' } };
    const { f, root } = render(null, () => of({ ...PAGE, tasks: [waiting] }));
    await settle(f);
    expect(root.querySelector('.waiting')?.textContent).toContain('Cache not configured');
    expect(root.querySelector('.meta')?.textContent).not.toContain('Cache not configured');
    expect(root.querySelectorAll('gr-row .value').length).toBe(0);
  });

  it('shows "-" for entry points when the evaluation failed before it produced entry points', async () => {
    const evaluationError = { ...row('hosts'), entry_points: { ok: 0, failing: 0, total: 0 }, delta: 0 };
    const { f, root } = render(null, () => of({ ...PAGE, tasks: [evaluationError] }));
    await settle(f);
    expect(root.querySelector('gr-row .value b')?.textContent?.trim()).toBe('-');
  });

  it('leaves the filter to the server when the url has no filter and puts Starred first for a user with starred tasks', async () => {
    const { f, tasks, root } = render(null, () => of(STARRED));
    await settle(f);
    expect(tasks).toHaveBeenCalledWith(null, 1, 10, 60);
    expect(chipLabels(root)).toEqual(['Starred 2', 'All 12', 'Failing 3']);
    expect(root.querySelector('gr-tab-switch button.is-selected')?.textContent).toContain('Starred');
  });

  it('drops a starred filter from the url when nothing is starred', async () => {
    const navigate = vi.spyOn(Router.prototype, 'navigate').mockResolvedValue(true);
    const { f } = render('starred', () => of({ ...PAGE, filter: 'starred', total: 0, tasks: [] }));
    await settle(f);
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { filter: null }, replaceUrl: true }));
  });

  it('writes the chip into the url and leaves loading to the url', () => {
    const { tasks, root } = render(null, () => of(PAGE));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    (root.querySelectorAll('gr-tab-switch button')[0] as HTMLElement).click();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { filter: 'all', page: null } }));
    expect(tasks).toHaveBeenCalledTimes(1);
  });

  it('reloads when the filter in the url changes', async () => {
    const { f, tasks, params, root } = render(null, () => of(STARRED));
    params.next(convertToParamMap({ filter: 'starred' }));
    await settle(f);
    expect(tasks).toHaveBeenLastCalledWith('starred', 1, 10, 60);
    expect(root.querySelector('gr-tab-switch button.is-selected')?.textContent).toContain('Starred');
    params.next(convertToParamMap({ filter: 'starred', other: 'x' }));
    expect(tasks).toHaveBeenCalledTimes(2);
  });

  it('drops a response that a newer filter overtook', () => {
    const responses = [new Subject<TasksPage>(), new Subject<TasksPage>()];
    let call = 0;
    const { f, params, root } = render(null, () => responses[call++]);
    params.next(convertToParamMap({ filter: 'failing' }));
    responses[0].next({ ...PAGE, tasks: [row('stale')] });
    responses[1].next({ ...PAGE, tasks: [row('fresh')] });
    f.detectChanges();
    expect(root.querySelector('gr-row-list')?.textContent).toContain('fresh');
    expect(root.querySelector('gr-row-list')?.textContent).not.toContain('stale');
  });

  it('drops a background reload that a filter change overtook', () => {
    const background = new Subject<TasksPage>();
    let call = 0;
    const { f, params, root } = render(null, () => (++call === 2 ? background : of({ ...PAGE, tasks: [row(`load${call}`)] })));
    f.componentInstance.reloadSilentlyKeepingContentOnError().subscribe();
    params.next(convertToParamMap({ filter: 'failing' }));
    background.next({ ...PAGE, tasks: [row('stale')] });
    f.detectChanges();
    expect(root.querySelector('gr-row-list')?.textContent).toContain('load3');
    expect(root.querySelector('gr-row-list')?.textContent).not.toContain('stale');
  });

  it('passed and duration labels appear once in header row', () => {
    const { root } = render(null, () => of(PAGE));
    const header = Array.from(root.querySelectorAll('.columns span')).map((s) => s.textContent?.trim());
    expect(header).toEqual(['', 'passed', 'average time']);
    const values = Array.from(root.querySelectorAll('gr-row .value')).map((c) => [
      c.querySelector('b')?.textContent?.trim(),
      c.querySelector('small.sr-only')?.textContent?.trim(),
    ]);
    expect(values).toEqual([['41/45', 'Entry points passed'], ['14m 00s', 'average time']]);
  });

  it('names the full project and task on a truncated task name', () => {
    const link = render(null, () => of(PAGE)).root.querySelector<HTMLElement>('gr-row .name')!;
    expect(link.title).toBe('infra / hosts');
  });

  it('stretches one link over the row that opens the task', () => {
    const row = render(null, () => of(PAGE)).root.querySelector('gr-row')!;
    const links = row.querySelectorAll<HTMLAnchorElement>('a.row-link');
    expect(links.length).toBe(1);
    expect(links[0].getAttribute('href')).toBe('/project/infra/task/hosts');
    expect(row.querySelector('.row')?.classList).toContain('is-link');
  });

  it('offers View all only when more rows exist and writes the first page into the url', () => {
    const { root } = render(null, () => of(PAGE));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    expect(root.querySelector('.show-all')?.textContent).toContain('View all 12');
    (root.querySelector('.show-all') as HTMLElement).click();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { page: 1 } }));
    TestBed.resetTestingModule();
    expect(render(null, () => of({ ...PAGE, total: 1 })).root.querySelector('.show-all')).toBeNull();
  });

  it('pages by 25 from the page in the url and goes back to the top 10 on Show less', () => {
    const { tasks, root } = render(null, () => of(PAGE), { page: '1' });
    expect(tasks).toHaveBeenLastCalledWith(null, 1, 25, 60);
    expect(root.querySelector('.show-all')).toBeNull();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    (root.querySelector('.show-less') as HTMLElement).click();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { page: null } }));
  });

  it('ignores a page in the url that is not a positive whole number', () => {
    const { tasks } = render(null, () => of(PAGE), { page: 'x' });
    expect(tasks).toHaveBeenLastCalledWith(null, 1, 10, 60);
  });

  it('loads the tasks once and shows the newest bars that fit a narrow column', () => {
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(140);
    const history = Array.from({ length: 60 }, (_, i) => ({ id: `e${i}`, status: 'Completed' as const, created_at: '2026-09-23T10:00:00' }));
    const { f, tasks, root } = render(null, () => of({ ...PAGE, tasks: [{ ...row('hosts'), history }] }));
    f.detectChanges();
    f.detectChanges();
    expect(tasks).toHaveBeenCalledTimes(1);
    const bars = root.querySelectorAll<HTMLAnchorElement>('.history .bar');
    expect(bars.length).toBe(20);
    expect(bars[19].getAttribute('href')).toContain('e59');
  });

  it('holds the list with placeholder rows until the tasks arrive', async () => {
    const { f, root } = render(null, () => NEVER);
    await settle(f);
    const list = root.querySelector('gr-row-list')!;
    expect(list.getAttribute('aria-busy')).toBe('true');
    expect(list.querySelectorAll('gr-row').length).toBeGreaterThan(0);
    expect(list.querySelector('gr-row a')).toBeNull();
    expect(list.querySelector('gr-skeleton')).not.toBeNull();
  });

  it('shows an inline error and loads again on retry', () => {
    let calls = 0;
    const { f, root } = render(null, () => (++calls === 1 ? throwError(() => ({ status: 500 })) : of(PAGE)));
    expect(root.querySelector('gr-row-list')).toBeNull();
    (root.querySelector('gr-message-banner button') as HTMLElement).click();
    f.detectChanges();
    expect(root.querySelector('gr-message-banner')).toBeNull();
    expect(root.querySelectorAll('gr-row').length).toBe(1);
  });

  it('hides the whole block when the user sees no task at all', () => {
    const { root } = render(null, () => of({ ...PAGE, counts: { all: 0, failing: 0, starred: 0 }, total: 0, tasks: [] }));
    expect(root.textContent!.trim()).toBe('');
  });

  it('drops the column captions over a filter without tasks and names the empty filter', () => {
    const { root } = render('failing', () => of({ ...PAGE, counts: { all: 12, failing: 0, starred: 0 }, total: 0, tasks: [] }));
    expect(chipLabels(root)).toEqual(['All 12', 'Failing 0']);
    expect(root.querySelector('.columns')).toBeNull();
    expect(root.querySelector('gr-empty-state')?.textContent).toContain('No failing tasks');
  });

  it('hides the whole block on 403', () => {
    const { root } = render(null, () => throwError(() => ({ status: 403 })));
    expect(root.textContent!.trim()).toBe('');
  });
});
