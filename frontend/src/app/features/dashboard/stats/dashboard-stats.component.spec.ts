/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { DashboardStatsComponent } from './dashboard-stats.component';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardStats } from '@core/models';

const STATS: DashboardStats = {
  cpu_time_ms: 18.4 * 365 * 24 * 3_600_000,
  cpu_time_ms_7d: 0,
  builds_completed: 1_284_000,
  cache_size_bytes: 2.4 * 1024 ** 4,
  workers: { online: 7, busy_pct: 72 },
  queue_wait_p50_ms: 38_000,
};

const cards = (root: HTMLElement) =>
  Object.fromEntries(
    Array.from(root.querySelectorAll('gr-stat-card')).map((c) => [
      c.querySelector('.stat-label')!.textContent!.trim(),
      c.querySelector('.stat-value')!.textContent!.trim(),
    ]),
  );

function render(stats: () => Observable<DashboardStats>) {
  TestBed.configureTestingModule({
    imports: [DashboardStatsComponent],
    providers: [{ provide: DashboardService, useValue: { stats: vi.fn(stats) } }],
  });
  const f = TestBed.createComponent(DashboardStatsComponent);
  f.detectChanges();
  return { f, root: f.nativeElement as HTMLElement };
}

describe('DashboardStatsComponent', () => {
  it('reads the stats in human units, one card each', () => {
    expect(cards(render(() => of(STATS)).root)).toEqual({
      'CPU time': '18.4 y',
      Builds: '1.28M',
      'Cache size': '2.4 TiB',
      'Workers busy': '72%',
      'Avg. Queue wait': '38.0 s',
    });
  });

  it('shows an inline error and loads again on retry', () => {
    let calls = 0;
    const { f, root } = render(() => (++calls === 1 ? throwError(() => ({ status: 500 })) : of(STATS)));
    expect(root.querySelector('gr-message-banner')).not.toBeNull();
    (root.querySelector('gr-message-banner button') as HTMLElement).click();
    f.detectChanges();
    expect(root.querySelector('gr-message-banner')).toBeNull();
    expect(cards(root)['CPU time']).toBe('18.4 y');
  });

  it('hides itself on 403', () => {
    const { root } = render(() => throwError(() => ({ status: 403 })));
    expect(root.textContent!.trim()).toBe('');
  });

  it('shows a dash for workers busy when no worker is online', () => {
    expect(cards(render(() => of({ ...STATS, workers: { online: 0, busy_pct: 0 } })).root)['Workers busy']).toBe('-');
  });
});
