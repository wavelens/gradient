/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { NEVER, of, throwError } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';
import { BoardOverviewComponent } from './overview.component';

function setup(completed = of([{ bucket_start: '2026-11-09T10:00:00Z', count: 3 }])) {
  TestBed.configureTestingModule({
    imports: [BoardOverviewComponent],
    providers: [
      { provide: BoardLiveService, useValue: { connect: () => NEVER } },
      {
        provide: BoardService,
        useValue: {
          getWorkers: () => throwError(() => new Error('Internal server error')),
          getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
          getAssignedJobs: () => of({ jobs: [{}], other_running: 1 }),
          query: () => completed,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardOverviewComponent);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('BoardOverviewComponent', () => {
  it('says a failed count could not load instead of showing zero', () => {
    const cards = Array.from(setup().querySelectorAll('.kpis gr-stat-card')).map((c) => c.textContent?.trim());
    expect(cards[0]).toContain("Couldn't load");
    expect(cards[1]).toContain('0');
    expect(cards[2]).toContain('1 (1 hidden)');
  });

  it('says the completed builds chart failed instead of No data', () => {
    const el = setup(throwError(() => new Error('Internal server error')));
    expect(el.querySelector('gr-metric-chart')?.textContent).toContain("Couldn't load");
    expect(el.querySelector('gr-metric-chart')?.textContent).not.toContain('No data');
  });
});
