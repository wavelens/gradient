/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';
import type { BoardNetworkStats, HttpRouteStat } from '@core/models';
import { BoardNetworkComponent, getHourlySeriesPointsWithoutCurrentBucket } from './network.component';

const route = (i: number): HttpRouteStat => ({ method: 'GET', route: `/api/v1/r${i}`, count: 100 - i, avg_ms: 5, errors: 0 });

const STATS: BoardNetworkStats = {
  nar_egress: [],
  workers: [],
  http: Array.from({ length: 25 }, (_, i) => route(i)),
};

function setup(stats: BoardNetworkStats = STATS) {
  TestBed.configureTestingModule({
    imports: [BoardNetworkComponent],
    providers: [
      { provide: BoardService, useValue: { getNetwork: () => of(stats) } },
      { provide: AuthService, useValue: { user: () => ({ superuser: true }) } },
    ],
  });
  const fixture = TestBed.createComponent(BoardNetworkComponent);
  fixture.detectChanges();
  return fixture;
}

describe('BoardNetworkComponent - HTTP routes', () => {
  it('shows the busiest ten routes and loads the rest on request', () => {
    const fixture = setup();
    const el = fixture.nativeElement as HTMLElement;
    const rows = () => el.querySelectorAll('gr-table.http tbody tr');
    expect(rows().length).toBe(10);
    expect(rows()[0].textContent).toContain('/api/v1/r0');
    expect(el.querySelector('gr-load-more')?.textContent).toContain('Showing 10 of 25');

    (el.querySelector('gr-load-more button') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(rows().length).toBe(20);
  });
});

describe('BoardNetworkComponent - worker speeds', () => {
  it('shows No data instead of zero bars when no worker reports a disk speed', () => {
    const fixture = setup({ ...STATS, workers: [{ worker_id: 'w1', worker_name: 'w1', upload_speed_mbps: 5, disk_speed_mbps: null }] });
    const component = fixture.componentInstance;
    expect(component.diskSeries()[0].data).toEqual([null]);
    expect(component.transferSeries()[1].data).toEqual([null]);
    expect(component.transferSeries()[0].data).toEqual([5]);
  });
});

describe('getHourlySeriesPointsWithoutCurrentBucket', () => {
  it('drops the hour that is still running so egress does not appear to fall to zero', () => {
    const now = Date.parse('2026-01-01T12:30:00Z');
    const points = [
      { bucket_start: '2026-01-01T11:00:00Z', count: 3, sum: 57 },
      { bucket_start: '2026-01-01T12:00:00Z', count: 0, sum: 0 },
    ];
    expect(getHourlySeriesPointsWithoutCurrentBucket(points, now)).toEqual([points[0]]);
  });
});
