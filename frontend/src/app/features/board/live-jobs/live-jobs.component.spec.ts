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
          getAssignmentDecisions: () => of([]),
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

function setupWithAssigned(assigned: DispatchedJobSummary[], superuser = false, otherRunning = 0): ComponentFixture<BoardLiveJobsComponent> {
  TestBed.configureTestingModule({
    imports: [BoardLiveJobsComponent],
    providers: [
      provideRouter([]),
      {
        provide: BoardService,
        useValue: {
          getAssignedJobs: () => of({ jobs: assigned, other_running: otherRunning }),
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
    const exec_job = { id: 'x1', project: 'infra', task: 'nightly', task_display_name: 'Nightly', entry_point: 'apps.x86_64-linux.train', node_index: 1, nodes: 2 };
    const fixture = setupWithAssigned(
      [{ ...ASSIGNED, kind: 2, build_id: null, score: 7.25, worker_name: 'Builder One', subject: exec_job.entry_point, exec_job }],
      true,
    );
    fixture.detectChanges();
    const cells = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('tbody tr td'))
      .map((cell) => cell.textContent?.trim());
    expect(cells.slice(0, 5)).toEqual(['exec', '-', 'Builder One', 'apps.x86_64-linux.train (node 1)', '7.3']);
  });

  it('hides the score column and score filters from a non-superuser and ignores a score filter', () => {
    const fixture = setupWithAssigned([ASSIGNED]);
    fixture.componentInstance.scoreMin.set(100);
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    expect(Array.from(el.querySelectorAll('thead th')).map((th) => th.textContent?.trim())).not.toContain('Score');
    expect(el.querySelector('.filters')?.textContent).not.toContain('Score');
    expect(el.querySelectorAll('tbody tr td').length).toBe(6);
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
      .map((row) => row.querySelector('td.worker')?.textContent?.trim());
    expect(workers).toEqual(['Builder One', 'Not shared with this account']);
  });

  it('lists the nodes of a multi-node job as one cluster job that opens the cluster page', () => {
    const node = (index: number) => ({ id: `x${index}`, project: 'infra', task: 'nightly', task_display_name: 'Nightly', entry_point: 'apps.x86_64-linux.train', node_index: index, nodes: 2 });
    const fixture = setupWithAssigned([
      { ...ASSIGNED, id: 'j0', kind: 2, build_id: null, exec_job: node(0), cluster_job: 'c1' },
      { ...ASSIGNED, id: 'j1', kind: 2, build_id: null, exec_job: node(1), cluster_job: 'c1' },
    ]);
    fixture.detectChanges();
    const rows = (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLElement>('tbody tr');
    expect(rows.length).toBe(1);
    expect(Array.from(rows[0].querySelectorAll('td')).slice(2, 4).map((cell) => cell.textContent?.trim()))
      .toEqual(['2 sub jobs', 'apps.x86_64-linux.train (cluster, 2 sub jobs)']);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    rows[0].click();
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/board/clusters/c1');
  });

  it('counts the running jobs and names how many a filter shows', () => {
    const fixture = setupWithAssigned([ASSIGNED, { ...ASSIGNED, id: 'x', kind: 2, build_id: null }]);
    const banner = () => (fixture.nativeElement as HTMLElement).querySelector('.banner')?.textContent?.trim();
    expect(banner()).toBe('2 running jobs');
    fixture.componentInstance.kindFilter.set('exec');
    fixture.detectChanges();
    expect(banner()).toBe('1 of 2 running jobs');
  });

  it('says once that no job runs, also with a filter set', () => {
    const fixture = setupWithAssigned([]);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.banner')?.textContent?.trim()).toBe('No running jobs');
    expect(root.querySelector('gr-table')).toBeNull();
    fixture.componentInstance.kindFilter.set('exec');
    fixture.detectChanges();
    expect(root.querySelector('gr-table')).toBeNull();
  });

  it('says when a filter hides every running job', () => {
    const fixture = setupWithAssigned([ASSIGNED]);
    fixture.componentInstance.kindFilter.set('exec');
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).querySelector('tbody td.muted')?.textContent?.trim()).toBe('No matching running jobs.');
  });

  it('counts the jobs on projects the user cannot view in the same line', () => {
    const fixture = setupWithAssigned([], false, 1);
    expect((fixture.nativeElement as HTMLElement).querySelector('.banner')?.textContent?.trim())
      .toBe("No running jobs you can see (1 on projects you can't view)");
  });

  it('shows how long ago a job was assigned, with the exact time as its title', () => {
    vi.useFakeTimers({ now: new Date('2026-06-08T00:07:00Z') });
    try {
      const cell = (setupWithAssigned([ASSIGNED]).nativeElement as HTMLElement).querySelector('tbody td.age') as HTMLElement;
      expect(cell.textContent?.trim()).toContain('7m ago');
      expect(cell.title).not.toBe('');
    } finally {
      vi.useRealTimers();
    }
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

  it('surfaces passed-over and negative candidate scores when scope is "incl. passed over"', () => {
    const fixture = setupDecisions();
    const cmp = fixture.componentInstance;
    cmp.setScoreScope('all');
    fixture.detectChanges();

    const rows = cmp.decisionRows();
    expect(rows.length).toBe(2);
    expect(rows.some((r) => r.score < 0 && r.subject === 'loser' && !r.won)).toBe(true);
    expect(rows.some((r) => r.subject === 'win' && r.won)).toBe(true);

    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Passed over');
    expect(text).toContain('-8');
    expect(rows.every((r) => r.worker === 'Builder One')).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.banner')?.textContent?.trim()).toBe('No running jobs');
  });

  it('caps the decision rows and grows them with Load more', () => {
    const many: AssignDecisionView[] = Array.from({ length: 30 }, (_, i) => ({
      ...DECISIONS[0],
      at: `2026-06-08T00:${String(i).padStart(2, '0')}:00Z`,
    }));
    const fixture = setupDecisions();
    getAssignmentDecisions.mockReturnValueOnce(of(many));
    fixture.componentInstance.setScoreScope('all');
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelectorAll('gr-table.jobs tbody tr').length).toBe(50);
    expect(root.querySelector('gr-load-more')?.textContent).toContain('Showing 50 of 60');
    (root.querySelector('gr-load-more button') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(root.querySelectorAll('gr-table.jobs tbody tr').length).toBe(60);
  });

  it('hides the decision scores from a non-superuser and never asks for them', () => {
    const fixture = setupDecisions(false);
    fixture.componentInstance.setScoreScope('all');
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).not.toContain('incl. passed over');
    expect(text).not.toContain('superuser');
    expect(text).toContain('No running jobs');
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

  it('opens and keeps the filters named in the address', () => {
    TestBed.overrideProvider(AuthService, { useValue: { user: () => ({ superuser: true }) } });
    const fixture = setup({ type: 'exec', min: '5', max: 'oops', scores: 'all' });
    const cmp = fixture.componentInstance;
    expect([cmp.kindFilter(), cmp.scoreMin(), cmp.scoreMax(), cmp.scoreScope()]).toEqual(['exec', 5, null, 'all']);

    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    cmp.scoreMax.set(9.5);
    expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ queryParams: { max: '9.5' } }));
    cmp.scoreMin.set(null);
    expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ queryParams: { min: null } }));
    expect(cmp.scoreMin()).toBeNull();
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
    const exec_job = { id: 'x1', project: 'infra', task: 'nightly', task_display_name: 'Nightly', entry_point: 'apps.x86_64-linux.train', node_index: 0, nodes: 1 };
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
    expect(row.querySelector('td.waiting')?.textContent).toContain('No writable cache');
  });

  it('counts the dependencies a pending build waits for when the server names no other reason', () => {
    expect(pendingRow(PENDING).querySelector('td.waiting')?.textContent?.trim()).toBe('Waiting for 3 dependencies');
    TestBed.resetTestingModule();
    expect(pendingRow({ ...PENDING, dependency_count: 1 }).querySelector('td.waiting')?.textContent?.trim()).toBe('Waiting for 1 dependency');
    TestBed.resetTestingModule();
    expect(pendingRow({ ...PENDING, dependency_count: 0 }).querySelector('td.waiting')?.textContent?.trim()).toBe('Queued, waiting for the scheduler');
  });

  it('names the project and task of a pending row by their display names', () => {
    const row = pendingRow({
      ...PENDING,
      project_name: 'sandbox',
      project_display_name: 'Sandbox',
      task: 'execstates',
      task_display_name: 'Exec States Task',
    });
    expect(row.querySelector('td.origin')?.textContent?.replace(/\s+/g, ' ').trim()).toBe('Sandbox / Exec States Task');
  });

  it('names the project and task of an assigned row by their display names', () => {
    const fixture = setupWithAssigned([{ ...ASSIGNED, project_name: 'sandbox', project_display_name: 'Sandbox', task: 'oktask', task_display_name: 'OK Task' }]);
    const cell = fixture.nativeElement.querySelector('tbody td.origin') as HTMLElement;
    expect(cell.textContent?.replace(/\s+/g, ' ').trim()).toBe('Sandbox / OK Task');
    expect(Array.from(cell.querySelectorAll('a')).map((a) => a.getAttribute('href'))).toEqual(['/project/sandbox', '/project/sandbox/task/oktask?eval=e1abc123']);
  });

  it('shows a hint instead of the action to a viewer who may not fix the waiting reason', () => {
    const row = pendingRow({ ...PENDING, project_name: 'sandbox', waiting_reason: { kind: 'no_cache' }, permissions: ['viewProject'] });

    expect(row.querySelector('td.waiting a')).toBeNull();
    expect(row.querySelector('td.waiting .hint')?.textContent).toContain('Ask a project admin');
  });

  it('lists pending jobs oldest first', () => {
    TestBed.configureTestingModule({
      imports: [BoardLiveJobsComponent],
      providers: [
        provideRouter([]),
        {
          provide: BoardService,
          useValue: {
            getAssignedJobs: () => of({ jobs: [], other_running: 0 }),
            getPendingJobs: () => of({
              jobs: [
                { ...PENDING, build_id: 'b-new', subject: 'newest', queued_at: '2026-06-08T00:05:00Z' },
                { ...PENDING, build_id: 'b-old', subject: 'oldest', queued_at: '2026-06-07T23:00:00Z' },
                { ...PENDING, build_id: 'b-mid', subject: 'middle', queued_at: '2026-06-08T00:01:00Z' },
              ],
              other_pending: 0,
            }),
          },
        },
        { provide: BoardLiveService, useValue: { connect: () => EMPTY } },
        authAs(false),
      ],
    });
    const fixture = TestBed.createComponent(BoardLiveJobsComponent);
    fixture.componentInstance.setView('pending');
    fixture.detectChanges();
    const subjects = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('tbody td.subject')).map((c) => c.textContent?.trim());
    expect(subjects).toEqual(['oldest', 'middle', 'newest']);
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
    expect(text).toContain("1 pending job (2 on projects you can't view)");
  });
});
