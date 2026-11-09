/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { NEVER, of, throwError } from 'rxjs';
import { DashboardActivityComponent } from './dashboard-activity.component';
import { signal } from '@angular/core';
import { AuthService } from '@core/services/auth.service';
import { DashboardService } from '@core/services/dashboard.service';
import { ActivityDay } from '@core/models';

function render(activity: () => ReturnType<DashboardService['activity']>) {
  const spy = vi.fn(activity);
  TestBed.configureTestingModule({
    imports: [DashboardActivityComponent],
    providers: [{ provide: DashboardService, useValue: { activity: spy } }, { provide: AuthService, useValue: { user: signal(null) } }],
  });
  const f = TestBed.createComponent(DashboardActivityComponent);
  f.detectChanges();
  return { f, spy, root: f.nativeElement as HTMLElement };
}

const DAYS: ActivityDay[] = [
  { date: '2026-09-22', evaluations: 4, failed: 0 },
  { date: '2026-09-23', evaluations: 2, failed: 2 },
];

describe('DashboardActivityComponent', () => {
  it('switches between evaluations and failures without refetching', () => {
    const { f, spy, root } = render(() => of({ days: DAYS }));
    expect(root.querySelector('.hint')!.textContent).toContain('6 evaluations');
    (root.querySelectorAll('gr-tab-switch button')[1] as HTMLElement).click();
    f.detectChanges();
    expect(root.querySelector('.hint')!.textContent).toContain('2 failed evaluations');
    expect(spy).toHaveBeenCalledTimes(1);
    expect(root.querySelectorAll('rect.day').length).toBe(2);
  });

  it('shades a day by its share of the busiest day', () => {
    const { root } = render(() => of({ days: DAYS }));
    const levels = Array.from(root.querySelectorAll('rect.day')).map((r) => r.getAttribute('class'));
    expect(levels).toEqual(['day day--4', 'day day--3']);
  });

  it('names each day in a native title, so the blocks add no tab stops', () => {
    const { root } = render(() => of({ days: DAYS }));
    const day = root.querySelectorAll('rect.day')[1];
    expect(day.querySelector('title')!.textContent).toBe('2026-09-23: 2 evaluations, 2 failed');
    expect(day.hasAttribute('tabindex')).toBe(false);
  });

  it('holds the heatmap space with a placeholder until the days arrive', () => {
    const { root } = render(() => NEVER);
    expect(root.querySelector('svg')).toBeNull();
    expect(root.querySelector('gr-skeleton.heat')).not.toBeNull();
    expect(root.querySelector('section')?.getAttribute('aria-busy')).toBe('true');
  });

  it('leaves a failed load to the page banner and hides itself on 403', () => {
    const { f, root } = render(() => throwError(() => ({ status: 500 })));
    expect(f.componentInstance.failed()).toBe(true);
    expect(root.textContent!.trim()).toBe('');
    TestBed.resetTestingModule();
    expect(render(() => throwError(() => ({ status: 403 }))).root.textContent!.trim()).toBe('');
  });

  it('describes the heatmap to assistive technology', () => {
    const { f, root } = render(() => of({ days: DAYS }));
    const svg = () => root.querySelector('svg')!;
    expect(svg().getAttribute('role')).toBe('img');
    expect(svg().getAttribute('aria-label')).toBe('Daily evaluations over the last 2 days: 6 in total, busiest day 2026-09-22 with 4');
    (root.querySelectorAll('gr-tab-switch button')[1] as HTMLElement).click();
    f.detectChanges();
    expect(svg().getAttribute('aria-label')).toBe('Daily failed evaluations over the last 2 days: 2 in total, busiest day 2026-09-23 with 2');
  });
});
