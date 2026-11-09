/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { BoardLiveJobsComponent } from './live-jobs.component';
import { BoardService } from '@core/services/board.service';
import { BoardLiveService } from '@core/services/board-live.service';
import { AuthService } from '@core/services/auth.service';
import { PendingJobSummary, DispatchedJobSummary, AssignDecisionView } from '@core/models';
import { links } from '@shared/routing';

function authAs(superuser: boolean) {
  return { provide: AuthService, useValue: { user: () => ({ superuser }) } };
}

const PENDING: PendingJobSummary = {
  kind: 1,
  project: 'o1',
  evaluation_id: 'e1abc123',
  build_id: 'b1',
  queued_at: '2026-06-08T00:00:00Z',
  dependency_count: 3,
  subject: null,
  permissions: [],
};

function setup(
  query: Record<string, string> = {},
  getPendingJobs: () => Observable<{ jobs: PendingJobSummary[]; other_pending: number }> = () => of({ jobs: [PENDING], other_pending: 2 }),
): ComponentFixture<BoardLiveJobsComponent> {
  TestBed.configureTestingModule({
    imports: [BoardLiveJobsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(query) } } },
      {
        provide: BoardService,
        useValue: {
          getAssignedJobs: () => of({ jobs: [ASSIGNED], other_running: 0 }),
          getPendingJobs,
        },
      },
      {
        provide: BoardLiveService,
        useValue: { connect: () => EMPTY },
      },
      authAs(false),
    ],
  });
  const fixture = TestBed.createComponent(BoardLiveJobsComponent);
  fixture.detectChanges();
  return fixture;
}

const ASSIGNED: DispatchedJobSummary = {
  id: 'd1abc123-0000-0000-0000-000000000001',
  kind: 1,
  project: 'o1',
  worker_id: 'worker-1',
  worker_name: null,
  score: 42.0,
  dispatched_at: '2026-06-08T00:00:00Z',
  build_id: 'b1',
  evaluation_id: 'e1abc123',
  subject: 'hello-2.12.1',
};

function setupWithAssigned(assigned: DispatchedJobSummary[], superuser = false): ComponentFixture<BoardLiveJobsComponent> {
  TestBed.configureTestingModule({
    imports: [BoardLiveJobsComponent],
    providers: [
      provideRouter([]),
      {
        provide: BoardService,
        useValue: {
          getAssignedJobs: () => of({ jobs: assigned, other_running: 0 }),
          getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
        },
      },
      {
        provide: BoardLiveService,
        useValue: { connect: () => EMPTY },
      },
      authAs(superuser),
    ],
  });
  const fixture = TestBed.createComponent(BoardLiveJobsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('BoardLiveJobsComponent - assigned subject column', () => {
  const subjectCell = (fixture: ComponentFixture<BoardLiveJobsComponent>) =>
    (fixture.nativeElement as HTMLElement).querySelector('tbody td.subject')?.textContent?.trim();

  it('names the derivation of a build job', () => {
    const fixture = setupWithAssigned([ASSIGNED]);
    fixture.detectChanges();
    expect(subjectCell(fixture)).toBe('hello-2.12.1');
  });

  it('names the repository of an eval job', () => {
    const fixture = setupWithAssigned([{ ...ASSIGNED, kind: 0, build_id: null, subject: 'https://git.example/acme.git' }]);
    fixture.detectChanges();
    expect(subjectCell(fixture)).toBe('https://git.example/acme.git');
  });

  it('names an exec job by entry point and node, with its score', () => {
    const exec_job = { id: 'x1', project: 'infra', task: 'nightly', entry_point: 'apps.x86_64-linux.train', node_index: 1, nodes: 2 };
    const fixture = setupWithAssigned(
      [{ ...ASSIGNED, kind: 2, build_id: null, score: 7.25, worker_name: 'Builder One', subject: exec_job.entry_point, exec_job }],
      true,
    );
    fixture.detectChanges();
    const cells = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('tbody tr td'))
      .map((cell) => cell.textContent?.trim());
    expect(cells.slice(0, 4)).toEqual(['exec', 'Builder One', 'apps.x86_64-linux.train (node 1)', '7.3']);
  });

  it('hides the score column and score filters from a non-superuser and ignores a score filter', () => {
    const fixture = setupWithAssigned([ASSIGNED]);
    fixture.componentInstance.scoreMin.set(100);
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    expect(Array.from(el.querySelectorAll('thead th')).map((th) => th.textContent?.trim())).not.toContain('Score');
    expect(el.querySelector('.filters')?.textContent).not.toContain('Score');
    expect(el.querySelectorAll('tbody tr td').length).toBe(5);
    expect(fixture.componentInstance.filteredJobs().length).toBe(1);
  });

  it('links the subject of an assigned row to its job page so the keyboard reaches it', () => {
    const fixture = setupWithAssigned([ASSIGNED]);
    fixture.detectChanges();
    const cells = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('tbody td.subject'));
    expect(cells[0].querySelector('a')?.getAttribute('href')).toBe(`/board/jobs/${ASSIGNED.id}`);
  });

  it('names an inverted score range instead of claiming no job matches', () => {
    const fixture = setupWithAssigned([ASSIGNED], true);
    fixture.componentInstance.scoreMin.set(5000);
    fixture.componentInstance.scoreMax.set(100);
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).querySelector('tbody td.muted')?.textContent?.trim()).toBe('Score min is above Score max.');
  });

  it('keeps exec jobs under the exec type filter only', () => {
    const fixture = setupWithAssigned([ASSIGNED, { ...ASSIGNED, id: 'x', kind: 2, build_id: null }]);
    const cmp = fixture.componentInstance;
    cmp.kindFilter.set('exec');
    expect(cmp.filteredJobs().map((j) => j.id)).toEqual(['x']);
    cmp.kindFilter.set('build');
    expect(cmp.filteredJobs().map((j) => j.id)).toEqual([ASSIGNED.id]);
  });

  it('heads the column for both kinds', () => {
    const fixture = setupWithAssigned([ASSIGNED]);
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Derivation / Evaluation');
  });

  it('shows the worker display name, or marks a worker the server does not name as not shared', () => {
    const fixture = setupWithAssigned([{ ...ASSIGNED, worker_name: 'Builder One' }, { ...ASSIGNED, id: 'j2' }]);
    fixture.detectChanges();
    const workers = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('tbody tr'))
      .map((row) => row.querySelectorAll('td')[1].textContent?.trim());
    expect(workers).toEqual(['Builder One', 'Not shared with this account']);
  });

  it('lists the nodes of a multi-node job as one cluster job that opens the cluster page', () => {
    const node = (index: number) => ({ id: `x${index}`, project: 'infra', task: 'nightly', entry_point: 'apps.x86_64-linux.train', node_index: index, nodes: 2 });
    const fixture = setupWithAssigned([
      { ...ASSIGNED, id: 'j0', kind: 2, build_id: null, exec_job: node(0), cluster_job: 'c1' },
      { ...ASSIGNED, id: 'j1', kind: 2, build_id: null, exec_job: node(1), cluster_job: 'c1' },
    ]);
    fixture.detectChanges();
    const rows = (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLElement>('tbody tr');
    expect(rows.length).toBe(1);
    expect(Array.from(rows[0].querySelectorAll('td')).slice(1, 3).map((cell) => cell.textContent?.trim()))
      .toEqual(['2 sub jobs', 'apps.x86_64-linux.train (cluster, 2 sub jobs)']);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    rows[0].click();
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/board/clusters/c1');
  });

  it('renders - when the subject is unknown', () => {
    const fixture = setupWithAssigned([{ ...ASSIGNED, subject: null }]);
    fixture.detectChanges();
    expect(subjectCell(fixture)).toBe('-');
  });
});

// Shaped by AssignmentDecisionView so the compiler catches drift: the server
// sends a per-candidate `id` and `won`, not just a decision-level `winner`.
const DECISIONS: AssignDecisionView[] = [
  {
    at: '2026-06-08T00:00:00Z',
    worker_id: 'w1',
    worker_name: 'Builder One',
    kind: 1,
    winner: 'j1',
    candidates: [
      { id: 'c1', job_id: 'j1', kind: 1, project: 'o1', build_id: 'b1', evaluation_id: 'e1', subject: 'win', score: 12, won: true },
      { id: 'c2', job_id: 'j2', kind: 1, project: 'o2', build_id: 'b2', evaluation_id: 'e2', subject: 'loser', score: -8, won: false },
    ],
  },
];

describe('BoardLiveJobsComponent - decision scores (#419)', () => {
  const getAssignmentDecisions = vi.fn(() => of(DECISIONS));

  function setupDecisions(superuser = true): ComponentFixture<BoardLiveJobsComponent> {
    getAssignmentDecisions.mockClear();
    TestBed.configureTestingModule({
      imports: [BoardLiveJobsComponent],
      providers: [
        provideRouter([]),
        {
          provide: BoardService,
          useValue: {
            getAssignedJobs: () => of({ jobs: [], other_running: 0 }),
            getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
            getAssignmentDecisions,
          },
        },
        { provide: BoardLiveService, useValue: { connect: () => EMPTY } },
        authAs(superuser),
      ],
    });
    const fixture = TestBed.createComponent(BoardLiveJobsComponent);
    fixture.detectChanges();
    return fixture;
  }

  it('surfaces rejected/negative candidate scores when scope is "incl. rejected"', () => {
    const fixture = setupDecisions();
    const cmp = fixture.componentInstance;
    cmp.setScoreScope('all');
    fixture.detectChanges();

    const rows = cmp.decisionRows();
    expect(rows.length).toBe(2);
    expect(rows.some((r) => r.score < 0 && r.subject === 'loser' && !r.won)).toBe(true);
    expect(rows.some((r) => r.subject === 'win' && r.won)).toBe(true);

    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('passed over');
    expect(text).toContain('-8');
    expect(rows.every((r) => r.worker === 'Builder One')).toBe(true);
  });

  it('hides the decision scores from a non-superuser and never asks for them', () => {
    const fixture = setupDecisions(false);
    fixture.componentInstance.setScoreScope('all');
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).not.toContain('incl. rejected');
    expect(text).not.toContain('superuser');
    expect(text).toContain('No matching assigned jobs.');
    expect(getAssignmentDecisions).not.toHaveBeenCalled();
  });
});

describe('BoardLiveJobsComponent - pending view toggle', () => {
  it('defaults to assigned view', () => {
    const fixture = setup();
    expect(fixture.componentInstance.view()).toBe('assigned');
  });

  it('opens the view named in the address', () => {
    const cmp = setup({ view: 'pending' }).componentInstance;
    expect(cmp.view()).toBe('pending');
    expect(cmp.pendingJobs().length).toBe(1);
  });

  it('writes the view to the address when switched', () => {
    const fixture = setup();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    fixture.componentInstance.setView('pending');
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { view: 'pending' } }));
  });

  it('keeps the switch and the assigned list reachable when the pending list fails to load', () => {
    const fixture = setup({ view: 'pending' }, () => throwError(() => new Error('down')));
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('gr-tab-switch')).not.toBeNull();
    expect(el.textContent).toContain('Failed to load jobs');

    fixture.componentInstance.setView('assigned');
    fixture.detectChanges();
    expect(el.textContent).not.toContain('Failed to load jobs');
    expect(el.querySelector('tbody td.subject')?.textContent?.trim()).toBe('hello-2.12.1');
  });

  it('lists a pending exec job and opens it by its exec job id', () => {
    const exec_job = { id: 'x1', project: 'infra', task: 'nightly', entry_point: 'apps.x86_64-linux.train', node_index: 0, nodes: 1 };
    TestBed.configureTestingModule({
      imports: [BoardLiveJobsComponent],
      providers: [
        provideRouter([]),
        {
          provide: BoardService,
          useValue: {
            getAssignedJobs: () => of({ jobs: [], other_running: 0 }),
            getPendingJobs: () => of({ jobs: [{ ...PENDING, kind: 2, build_id: null, exec_job }], other_pending: 0 }),
          },
        },
        { provide: BoardLiveService, useValue: { connect: () => EMPTY } },
        authAs(false),
      ],
    });
    const fixture = TestBed.createComponent(BoardLiveJobsComponent);
    fixture.componentInstance.setView('pending');
    fixture.detectChanges();

    const row = (fixture.nativeElement as HTMLElement).querySelector('tbody tr') as HTMLElement;
    const cells = Array.from(row.querySelectorAll('td'));
    expect([cells[0], cells[2]].map((c) => c.textContent?.trim())).toEqual(['exec', 'apps.x86_64-linux.train']);
    expect(cells[2].querySelector('a')?.getAttribute('href')).toBe('/board/pending/x1');

    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    cells[0].click();
    expect(navigate).toHaveBeenCalledWith(links.boardPendingJob({ evaluation_id: PENDING.evaluation_id, build_id: null, exec_job }));
  });

  function pendingRow(job: PendingJobSummary, superuser = false): HTMLElement {
    TestBed.configureTestingModule({
      imports: [BoardLiveJobsComponent],
      providers: [
        provideRouter([]),
        {
          provide: BoardService,
          useValue: {
            getAssignedJobs: () => of({ jobs: [], other_running: 0 }),
            getPendingJobs: () => of({ jobs: [job], other_pending: 0 }),
          },
        },
        { provide: BoardLiveService, useValue: { connect: () => EMPTY } },
        authAs(superuser),
      ],
    });
    const fixture = TestBed.createComponent(BoardLiveJobsComponent);
    fixture.componentInstance.setView('pending');
    fixture.detectChanges();
    return (fixture.nativeElement as HTMLElement).querySelector('tbody tr') as HTMLElement;
  }

  it('links a pending evaluation to its project and task and offers the next step for its waiting reason', () => {
    const row = pendingRow({
      ...PENDING,
      kind: 0,
      build_id: null,
      project_name: 'sandbox',
      task: 'oktask',
      waiting_reason: { kind: 'no_cache' },
      permissions: ['viewProject', 'manageSubscriptions'],
    });

    const hrefs = Array.from(row.querySelectorAll('a')).map((a) => a.getAttribute('href'));
    expect(hrefs).toEqual([
      '/project/sandbox',
      '/project/sandbox/task/oktask?eval=e1abc123',
      '/board/pending/e1abc123',
      '/project/sandbox/caches',
    ]);
    expect(row.querySelector('td.waiting')?.textContent).toContain('Cache not configured');
  });

  it('shows a hint instead of the action to a viewer who may not fix the waiting reason', () => {
    const row = pendingRow({ ...PENDING, project_name: 'sandbox', waiting_reason: { kind: 'no_cache' }, permissions: ['viewProject'] });

    expect(row.querySelector('td.waiting a')).toBeNull();
    expect(row.querySelector('td.waiting .hint')?.textContent).toContain('Ask a project admin');
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
