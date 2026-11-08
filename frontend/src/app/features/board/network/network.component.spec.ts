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
import { BoardNetworkComponent } from './network.component';

const route = (i: number): HttpRouteStat => ({ method: 'GET', route: `/api/v1/r${i}`, count: 100 - i, avg_ms: 5, errors: 0 });

const STATS: BoardNetworkStats = {
  nar_egress: [],
  workers: [],
  http: Array.from({ length: 25 }, (_, i) => route(i)),
};

function setup() {
  TestBed.configureTestingModule({
    imports: [BoardNetworkComponent],
    providers: [
      { provide: BoardService, useValue: { getNetwork: () => of(STATS) } },
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
