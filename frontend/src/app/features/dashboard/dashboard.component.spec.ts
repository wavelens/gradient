/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { DashboardComponent } from './dashboard.component';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
import { CommandPaletteService } from '@shared/chrome/command-palette/command-palette.service';

function render(rail: { projects: unknown[]; caches: unknown[]; operations: boolean }) {
  TestBed.configureTestingModule({
    imports: [DashboardComponent],
    providers: [
      provideRouter([]),
      { provide: StarsService, useValue: { set: () => of(true) } },
      { provide: DashboardService, useValue: {
        rail: () => of(rail),
        stats: () => of({ cpu_time_ms: 0, cpu_time_ms_7d: 0, builds_completed: 0, cache_size_bytes: 0, workers: { online: 0, busy_pct: 0 }, queue_wait_p50_ms: 0 }),
        tasks: () => of({ counts: { all: 0, failing: 0, worse: 0, starred: 0 }, total: 0, tasks: [] }),
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
    const root = render({ projects: [], caches: [], operations: false });
    expect(root.querySelector('app-dashboard-start')).not.toBeNull();
    expect(root.querySelector('app-dashboard-task-table')).toBeNull();
    expect(root.querySelector('app-dashboard-stats')).toBeNull();
    expect(root.querySelector('app-dashboard-rail')?.classList).toContain('hidden');
  });

  it('shows the router blocks once the user has something', () => {
    const root = render({ projects: [{ name: 'p', display_name: 'P', starred: false, tier: 'member', status: null, task_count: 1 }], caches: [], operations: false });
    for (const sel of ['app-dashboard-stats', 'app-dashboard-task-table', 'app-dashboard-activity', 'app-dashboard-rail', '.palette-trigger']) {
      expect(root.querySelector(sel)).not.toBeNull();
    }
    expect(root.querySelector('app-dashboard-start')).toBeNull();
  });

  it('opens the command palette from the search field', () => {
    const root = render({ projects: [], caches: [{ name: 'c', display_name: 'C', starred: false, nar_count: 0 }], operations: false });
    (root.querySelector('.palette-trigger') as HTMLElement).click();
    expect(TestBed.inject(CommandPaletteService).isOpen()).toBe(true);
  });
});
