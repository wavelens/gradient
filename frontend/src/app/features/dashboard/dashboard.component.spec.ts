/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { NEVER, Observable, of, throwError } from 'rxjs';
import type { Mock } from 'vitest';
import { DashboardComponent } from './dashboard.component';
import { signal } from '@angular/core';
import { AuthService } from '@core/services/auth.service';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';

type RailStub = { projects: unknown[]; caches: unknown[] };

let service: Record<'rail' | 'stats' | 'tasks' | 'activity', Mock>;

function render(rail: RailStub | Observable<RailStub>, pendingInvites = 0) {
  service = {
    rail: vi.fn(() => (rail instanceof Observable ? rail : of(rail))),
    stats: vi.fn(() => of({ cpu_time_ms: 0, cpu_time_ms_7d: 0, builds_completed: 0, cache_size_bytes: 0, workers: { online: 0, busy_pct: 0 }, queue_wait_p50_ms: 0 })),
    tasks: vi.fn(() => of({ counts: { all: 0, failing: 0, starred: 0 }, total: 0, tasks: [] })),
    activity: vi.fn(() => of({ days: [] })),
  };
  TestBed.configureTestingModule({
    imports: [DashboardComponent],
    providers: [
      provideRouter([]),
      { provide: PendingInvitesService, useValue: { count: () => pendingInvites } },
      { provide: StarsService, useValue: { set: () => of(true), changed$: NEVER } },
      { provide: DashboardService, useValue: service },
      { provide: AuthService, useValue: { user: signal(null) } },
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

  it('shows where to start while the user has caches but no project', () => {
    const root = render({ projects: [], caches: [{ name: 'c', display_name: 'C', starred: false, nar_count: 0 }] });
    expect(root.querySelector('app-dashboard-stats')).toBeNull();
    expect(root.querySelector('app-dashboard-task-table')).toBeNull();
    expect(root.querySelector('app-dashboard-start')).not.toBeNull();
  });

  it('loads the router blocks alongside the rail instead of waiting for it', () => {
    const root = render(NEVER);
    expect(root.querySelector('app-dashboard-stats')).not.toBeNull();
    expect(root.querySelector('app-dashboard-task-table')).not.toBeNull();
    expect(root.querySelector('app-dashboard-start')).toBeNull();
    expect(root.querySelector('gr-loading-spinner')).toBeNull();
  });

  it('points an invited user to the pending invitations', () => {
    const root = render({ projects: [], caches: [] }, 2);
    const banner = root.querySelector('app-pending-invitations-banner gr-message-banner');
    expect(banner?.textContent).toContain('2 pending invitations');
    expect(banner?.querySelector('a')?.getAttribute('href')).toBe('/settings/invites');
  });

  it('reloads stats, tasks, activity and rail while the tab stays open', () => {
    vi.useFakeTimers();
    try {
      render({ projects: [{ name: 'p', display_name: 'P', starred: false, tier: 'member', status: null, task_count: 1 }], caches: [] });
      vi.advanceTimersByTime(15_000);
      expect(service.stats).toHaveBeenCalledTimes(2);
      expect(service.tasks).toHaveBeenCalledTimes(2);
      expect(service.rail).toHaveBeenCalledTimes(2);
      expect(service.activity).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it('names the failed sections in one page banner and loads only those again on retry', () => {
    const root = render(throwError(() => ({ status: 503 })));
    expect(root.querySelectorAll('gr-message-banner').length).toBe(1);
    expect(root.querySelector('gr-message-banner')?.textContent).toContain("Couldn't load projects and caches.");
    (root.querySelector('gr-message-banner button') as HTMLElement).click();
    expect(service.rail).toHaveBeenCalledTimes(2);
    expect(service.stats).toHaveBeenCalledTimes(1);
  });

  it('shows no invitation banner without pending invitations', () => {
    expect(render({ projects: [], caches: [] }).querySelector('app-pending-invitations-banner gr-message-banner')).toBeNull();
  });
});
