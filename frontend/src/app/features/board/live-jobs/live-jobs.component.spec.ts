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
import { DispatchedJobSummary, PendingJobSummary } from '@core/services/board.service';

const PENDING: PendingJobSummary = {
  kind: 1,
  organization: 'o1',
  evaluation_id: 'e1abc123',
  build_id: 'b1',
  queued_at: '2026-06-08T00:00:00Z',
  dependency_count: 3,
  pname: null,
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

const DISPATCHED: DispatchedJobSummary = {
  id: 'd1abc123-0000-0000-0000-000000000001',
  kind: 1,
  organization: 'o1',
  worker_id: 'worker-1',
  score: 42.0,
  dispatched_at: '2026-06-08T00:00:00Z',
  build_id: 'b1',
  evaluation_id: 'e1abc123',
  pname: 'hello',
};

function setupWithDispatched(dispatched: DispatchedJobSummary[]): ComponentFixture<BoardLiveJobsComponent> {
  TestBed.configureTestingModule({
    imports: [BoardLiveJobsComponent],
    providers: [
      provideRouter([]),
      {
        provide: BoardService,
        useValue: {
          getDispatchedJobs: () => of({ jobs: dispatched, other_running: 0 }),
          getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
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

describe('BoardLiveJobsComponent - dispatched pname column', () => {
  beforeEach(() => sessionStorage.clear());

  it('renders pname in the Derivation column for a dispatched build job', () => {
    const fixture = setupWithDispatched([DISPATCHED]);
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('hello');
  });

  it('renders - when pname is null', () => {
    const fixture = setupWithDispatched([{ ...DISPATCHED, pname: null }]);
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('-');
  });
});

describe('BoardLiveJobsComponent - pending view toggle', () => {
  beforeEach(() => sessionStorage.clear());

  it('defaults to dispatched view', () => {
    const fixture = setup();
    expect(fixture.componentInstance.view()).toBe('dispatched');
  });

  it('restores the persisted view and filters from sessionStorage', () => {
    sessionStorage.setItem(
      'board.live-jobs.filters',
      JSON.stringify({ view: 'pending', kindFilter: 'build', statusFilter: 'all', scoreMin: null, scoreMax: null }),
    );
    const cmp = setup().componentInstance;
    expect(cmp.view()).toBe('pending');
    expect(cmp.kindFilter()).toBe('build');
    expect(cmp.pendingJobs().length).toBe(1);
  });

  it('persists the view to sessionStorage when switched', () => {
    const fixture = setup();
    fixture.componentInstance.setView('pending');
    fixture.detectChanges();
    const stored = JSON.parse(sessionStorage.getItem('board.live-jobs.filters') ?? '{}');
    expect(stored.view).toBe('pending');
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
