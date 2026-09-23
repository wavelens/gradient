/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { BehaviorSubject, Observable, Subject, of, throwError } from 'rxjs';
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

const PAGE: TasksPage = { counts: { all: 12, failing: 3, starred: 0 }, total: 12, tasks: [row('hosts')] };

function render(filter: string | null, page: () => Observable<TasksPage>) {
  const tasks = vi.fn(page);
  const params = new BehaviorSubject(convertToParamMap(filter ? { filter } : {}));
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
    expect(tasks).toHaveBeenCalledWith('failing', 1, 10, 30);
    const chips = Array.from(root.querySelectorAll('gr-tab-switch button')).map((c) => c.textContent?.replace(/\s+/g, ' ').trim());
    expect(chips).toEqual(['All 12', 'Failing 3', 'Starred 0']);
    expect(root.querySelector('gr-tab-switch button.is-selected')?.textContent).toContain('Failing');
  });

  it('writes the chip into the url and leaves loading to the url', () => {
    const { tasks, root } = render(null, () => of(PAGE));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    (root.querySelectorAll('gr-tab-switch button')[2] as HTMLElement).click();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { filter: 'starred' } }));
    expect(tasks).toHaveBeenCalledTimes(1);
  });

  it('reloads when the filter in the url changes', async () => {
    const { f, tasks, params, root } = render(null, () => of(PAGE));
    params.next(convertToParamMap({ filter: 'starred' }));
    await settle(f);
    expect(tasks).toHaveBeenLastCalledWith('starred', 1, 10, 30);
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

  it('labels entry points, change and speed without a header', () => {
    const values = Array.from(render(null, () => of(PAGE)).root.querySelectorAll('gr-row .value')).map((c) => [
      c.querySelector('b')?.textContent?.trim(),
      c.querySelector('small')?.textContent?.trim(),
    ]);
    expect(values).toEqual([['41/45', 'entry points'], ['+3', 'change'], ['14m 00s', 'speed']]);
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

  it('offers Show all only when more rows exist, then pages by 25', () => {
    const { f, tasks, root } = render(null, () => of(PAGE));
    (root.querySelector('.show-all') as HTMLElement).click();
    f.detectChanges();
    expect(tasks).toHaveBeenLastCalledWith('all', 1, 25, 30);
    TestBed.resetTestingModule();
    expect(render(null, () => of({ ...PAGE, total: 1 })).root.querySelector('.show-all')).toBeNull();
  });

  it('shrinks the history to what a narrow column fits and settles there', () => {
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(140);
    const { f, tasks } = render(null, () => of(PAGE));
    f.detectChanges();
    f.detectChanges();
    expect(tasks).toHaveBeenCalledTimes(2);
    expect(tasks).toHaveBeenLastCalledWith('all', 1, 10, 20);
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

  it('hides the whole block on 403', () => {
    const { root } = render(null, () => throwError(() => ({ status: 403 })));
    expect(root.textContent!.trim()).toBe('');
  });
});
