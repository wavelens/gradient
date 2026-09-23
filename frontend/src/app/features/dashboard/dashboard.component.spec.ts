/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { NEVER, Observable, of } from 'rxjs';
import { DashboardComponent } from './dashboard.component';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';

type RailStub = { projects: unknown[]; caches: unknown[] };

function render(rail: RailStub | Observable<RailStub>) {
  TestBed.configureTestingModule({
    imports: [DashboardComponent],
    providers: [
      provideRouter([]),
      { provide: StarsService, useValue: { set: () => of(true) } },
      { provide: DashboardService, useValue: {
        rail: () => (rail instanceof Observable ? rail : of(rail)),
        stats: () => of({ cpu_time_ms: 0, cpu_time_ms_7d: 0, builds_completed: 0, cache_size_bytes: 0, workers: { online: 0, busy_pct: 0 }, queue_wait_p50_ms: 0 }),
        tasks: () => of({ counts: { all: 0, failing: 0, starred: 0 }, total: 0, tasks: [] }),
        activity: () => of({ days: [] }),
      } },
    ],
  });
  const f = TestBed.createComponent(DashboardComponent);
  f.detectChanges();
  f.detectChanges();
  return f.nativeElement as HTMLElement;
}

describe('DashboardComponent', () => {
  it('routes a new user to the first steps only', () => {
    const root = render({ projects: [], caches: [] });
    expect(root.querySelector('app-dashboard-start')).not.toBeNull();
    expect(root.querySelector('app-dashboard-task-table')).toBeNull();
    expect(root.querySelector('app-dashboard-stats')).toBeNull();
    expect(root.querySelector('app-dashboard-rail')?.classList).toContain('hidden');
  });

  it('shows the router blocks once the user has something', () => {
    const root = render({ projects: [{ name: 'p', display_name: 'P', starred: false, tier: 'member', status: null, task_count: 1 }], caches: [] });
    for (const sel of ['app-dashboard-stats', 'app-dashboard-task-table', 'app-dashboard-activity', 'app-dashboard-rail']) {
      expect(root.querySelector(sel)).not.toBeNull();
    }
    expect(root.querySelector('app-dashboard-start')).toBeNull();
  });

  it('shows a loading line until the rail answers', () => {
    const root = render(NEVER);
    expect(root.querySelector('gr-loading-spinner')).not.toBeNull();
    expect(root.querySelector('app-dashboard-stats')).toBeNull();
    expect(root.querySelector('app-dashboard-start')).toBeNull();
  });
});
