/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { DashboardRailComponent } from './dashboard-rail.component';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
import { Rail } from '@core/models';

function mount(rail: () => Observable<Rail>) {
  TestBed.configureTestingModule({
    imports: [DashboardRailComponent],
    providers: [
      provideRouter([]),
      { provide: DashboardService, useValue: { rail: vi.fn(rail) } },
      { provide: StarsService, useValue: { set: () => of(true) } },
    ],
  });
  const f = TestBed.createComponent(DashboardRailComponent);
  const empty: boolean[] = [];
  f.componentInstance.empty.subscribe((e) => empty.push(e));
  f.detectChanges();
  return { f, root: f.nativeElement as HTMLElement, empty };
}

function render(rail: Rail) {
  return mount(() => of(rail)).root;
}

const hrefs = (root: HTMLElement) => Array.from(root.querySelectorAll('a[href]')).map((a) => a.getAttribute('href'));

describe('DashboardRailComponent', () => {
  it('links projects, nested tasks and caches directly', () => {
    const root = render({
      projects: [{ name: 'infra', display_name: 'Infra', starred: true, tier: 'starred_active', status: 'Failed', task_count: 2,
        tasks: [{ name: 'hosts', status: 'Failed' }] }],
      caches: [{ name: 'main', display_name: 'Main', starred: false, nar_count: 1200 }],
      operations: false,
    });
    expect(hrefs(root)).toContain('/project/infra');
    expect(hrefs(root)).toContain('/project/infra/task/hosts');
    expect(hrefs(root)).toContain('/caches/main');
    expect(root.textContent).not.toContain('Operations');
  });

  it('shows operations links for operators', () => {
    const root = render({ projects: [], caches: [], operations: true });
    expect(hrefs(root)).toEqual(expect.arrayContaining(['/board', '/board/workers', '/board/scheduler', '/board/health']));
  });

  it('keeps the order it received and counts tasks only when not nested', () => {
    const root = render({
      projects: [
        { name: 'b', display_name: 'B', starred: false, tier: 'active', status: null, task_count: 1 },
        { name: 'a', display_name: 'A', starred: false, tier: 'member', status: null, task_count: 3 },
      ],
      caches: [],
      operations: false,
    });
    const names = Array.from(root.querySelectorAll('section:first-child .row-name > .name')).map((n) => n.textContent!.trim());
    expect(names).toEqual(['B', 'A']);
    expect(root.textContent).toContain('1 task');
    expect(root.textContent).toContain('3 tasks');
  });

  it('reports an empty rail', () => {
    expect(mount(() => of({ projects: [], caches: [], operations: false })).empty).toEqual([true]);
  });

  it('shows an inline error and loads again on retry', () => {
    let calls = 0;
    const rail: Rail = { projects: [], caches: [], operations: true };
    const { f, root, empty } = mount(() => (++calls === 1 ? throwError(() => ({ status: 500 })) : of(rail)));
    expect(empty).toEqual([false]);
    (root.querySelector('gr-message-banner button') as HTMLElement).click();
    f.detectChanges();
    expect(root.querySelector('gr-message-banner')).toBeNull();
    expect(hrefs(root)).toContain('/board/workers');
  });

  it('hides itself on 403', () => {
    const { root, empty } = mount(() => throwError(() => ({ status: 403 })));
    expect(root.textContent!.trim()).toBe('');
    expect(empty).toEqual([false]);
  });
});
