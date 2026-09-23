/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import { DashboardActivityComponent } from './dashboard-activity.component';
import { DashboardService } from '@core/services/dashboard.service';
import { ActivityDay } from '@core/models';

function render(activity: () => ReturnType<DashboardService['activity']>) {
  const spy = vi.fn(activity);
  TestBed.configureTestingModule({
    imports: [DashboardActivityComponent],
    providers: [{ provide: DashboardService, useValue: { activity: spy } }],
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
    (root.querySelectorAll('.seg button')[1] as HTMLElement).click();
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

  it('shows an inline error with retry, and hides itself on 403', () => {
    const { root } = render(() => throwError(() => ({ status: 500 })));
    expect(root.querySelector('.error button')).not.toBeNull();
    TestBed.resetTestingModule();
    expect(render(() => throwError(() => ({ status: 403 }))).root.textContent!.trim()).toBe('');
  });
});
