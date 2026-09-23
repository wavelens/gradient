/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { DashboardTaskTableComponent } from './dashboard-task-table.component';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
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
  reliability: 0.82,
  evaluations_per_week: 31,
  history: [],
});

const PAGE: TasksPage = { counts: { all: 12, failing: 3, worse: 1, starred: 0 }, total: 12, tasks: [row('hosts')] };

function render(filter: string | null, page: () => Observable<TasksPage>) {
  const tasks = vi.fn(page);
  TestBed.configureTestingModule({
    imports: [DashboardTaskTableComponent],
    providers: [
      provideRouter([]),
      { provide: DashboardService, useValue: { tasks } },
      { provide: StarsService, useValue: { set: () => of(true) } },
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { queryParamMap: convertToParamMap(filter ? { filter } : {}) } },
      },
    ],
  });
  const f = TestBed.createComponent(DashboardTaskTableComponent);
  f.detectChanges();
  return { f, tasks, root: f.nativeElement as HTMLElement };
}

describe('DashboardTaskTableComponent', () => {
  it('reads the chip from the url and shows every count', () => {
    const { tasks, root } = render('failing', () => of(PAGE));
    expect(tasks).toHaveBeenCalledWith('failing', 1, 10, 30);
    const chips = Array.from(root.querySelectorAll('.chip')).map((c) => c.textContent?.replace(/\s+/g, ' ').trim());
    expect(chips).toEqual(['All 12', 'Failing 3', 'Got worse 1', 'Starred 0']);
    expect(root.querySelector('.chip--on')?.textContent).toContain('Failing');
  });

  it('writes the chip into the url and reloads with it', () => {
    const { tasks, root } = render(null, () => of(PAGE));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    (root.querySelectorAll('.chip')[2] as HTMLElement).click();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { filter: 'worse' } }));
    expect(tasks).toHaveBeenLastCalledWith('worse', 1, 10, 30);
  });

  it('renders entry points, delta, speed, reliability and evaluations per week', () => {
    const cells = Array.from(render(null, () => of(PAGE)).root.querySelectorAll('tbody td.num')).map((c) =>
      c.textContent?.trim(),
    );
    expect(cells).toEqual(['41/45', '+3', '14m 00s', '82%', '31']);
  });

  it('offers Show all only when more rows exist, then pages by 25', () => {
    const { f, tasks, root } = render(null, () => of(PAGE));
    (root.querySelector('.show-all') as HTMLElement).click();
    f.detectChanges();
    expect(tasks).toHaveBeenLastCalledWith('all', 1, 25, 30);
    TestBed.resetTestingModule();
    expect(render(null, () => of({ ...PAGE, total: 1 })).root.querySelector('.show-all')).toBeNull();
  });

  it('measures the history column and asks for as many bars as fit', () => {
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(140);
    const { f, tasks } = render(null, () => of(PAGE));
    f.detectChanges();
    expect(tasks).toHaveBeenCalledTimes(2);
    expect(tasks).toHaveBeenLastCalledWith('all', 1, 10, 20);
    vi.restoreAllMocks();
  });

  it('shows an inline error and loads again on retry', () => {
    let calls = 0;
    const { f, root } = render(null, () => (++calls === 1 ? throwError(() => ({ status: 500 })) : of(PAGE)));
    expect(root.querySelector('table')).toBeNull();
    (root.querySelector('.error button') as HTMLElement).click();
    f.detectChanges();
    expect(root.querySelector('.error')).toBeNull();
    expect(root.querySelectorAll('tbody tr').length).toBe(1);
  });

  it('hides the whole block on 403', () => {
    const { root } = render(null, () => throwError(() => ({ status: 403 })));
    expect(root.textContent!.trim()).toBe('');
  });
});
