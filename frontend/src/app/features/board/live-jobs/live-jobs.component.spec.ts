/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, EMPTY } from 'rxjs';
import { BoardLiveJobsComponent } from './live-jobs.component';
import { BoardService } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';
import { PendingJobSummary } from '@core/services/board.service';

const PENDING: PendingJobSummary = {
  kind: 1,
  organization: 'o1',
  evaluation_id: 'e1abc123',
  build_id: 'b1',
  queued_at: '2026-06-08T00:00:00Z',
  dependency_count: 3,
};

function setup(): ComponentFixture<BoardLiveJobsComponent> {
  TestBed.configureTestingModule({
    imports: [BoardLiveJobsComponent],
    providers: [
      provideRouter([]),
      {
        provide: BoardService,
        useValue: {
          getDispatchedJobs: () => of({ jobs: [], other_running: 0 }),
          getPendingJobs: () => of({ jobs: [PENDING], other_pending: 2 }),
        },
      },
      {
        provide: BoardLiveService,
        useValue: { connect: () => EMPTY },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardLiveJobsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('BoardLiveJobsComponent - pending view toggle', () => {
  it('defaults to dispatched view', () => {
    const fixture = setup();
    expect(fixture.componentInstance.view()).toBe('dispatched');
  });

  it('loads pending jobs when switching to pending view', () => {
    const fixture = setup();
    const cmp = fixture.componentInstance;
    cmp.setView('pending');
    expect(cmp.pendingJobs().map((j) => j.evaluation_id)).toEqual(['e1abc123']);
    expect(cmp.otherPending()).toBe(2);
  });

  it('renders pending job count in the banner after switching view', () => {
    const fixture = setup();
    const cmp = fixture.componentInstance;
    cmp.setView('pending');
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('pending job(s)');
  });
});
