/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router, RouterLink } from '@angular/router';
import { By, Title } from '@angular/platform-browser';
import { EMPTY, type Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { BoardJobDetailComponent, getPendingJobName } from './job-detail.component';
import { BoardService, AssignedJobDetail } from '@core/services/board.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { BoardExecJob, PendingJobSummary } from '@core/models';
import { formatDuration } from '@shared/text';

const DETAIL: AssignedJobDetail = {
  id: 'job-1',
  kind: 1,
  project: 'o1',
  project_name: 'project-one',
  project_display_name: 'Project One',
  worker_id: 'w1',
  worker_name: 'Builder One',
  can_open_worker_metrics_page: true,
  can_view_project: true,
  dispatched_at: '2026-06-08T00:01:00Z',
  build_id: null,
  evaluation_id: 'e1',
  evaluation: null,
  pname: null,
  queued_at: '2026-06-08T00:00:00Z',
  finished_at: null,
  ready_at: '2026-06-08T00:00:30Z',
  outcome: null,
  phases: [
    { seq: 0, parent_seq: null, phase: 'build', start_ms: 0, end_ms: 5000, paths: 0, bytes: 0 },
    { seq: 1, parent_seq: null, phase: 'compress', start_ms: 5000, end_ms: 6000, paths: 2, bytes: 0 },
    { seq: 2, parent_seq: 1, phase: 'nar_push', start_ms: 5100, end_ms: 5900, paths: 1, bytes: 1024 },
  ],
  worker_elapsed_ms: null,
  worker_tail_ms: null,
  transit_ms: null,
  derivation_build_id: 'a1',
  derivations: [{ build: 'bj1', derivation_build: 'a1', drv_path: '/nix/store/xxx-foo', pname: 'foo' }],
  assigned_scheduler_reasoning: {
    score: 12.5,
    score_breakdown: { rules: { wait: 3.5, missing: -1.2 }, total: 12.5 },
    worker_context: {
      architectures: ['x86_64-linux'],
      system_features: ['kvm'],
      capabilities: { core: true, federate: false, network: true, secrets: false, exec: false, eval: false, build: true, cache: true },
      cpu_count: 8,
      cpu_core_score: 1.5,
      ram_total_mb: 32000,
      ram_free_mb: 16000,
      cpu_usage_pct: 42,
      disk_speed_mbps: null,
      upload_speed_mbps: 940,
      download_speed_mbps: null,
    },
    job_context: {
      kind: 'Build',
      architecture: 'x86_64-linux',
      missing_count: 5,
      missing_nar_size: 1024,
      outputs_present: false,
      project_work_share: 0.25,
      rescore_count: 2,
      queued_at: '2026-06-08T00:00:00Z',
      ready_at: '2026-06-08T00:00:30Z',
      dependency_count: 3,
      pname: 'foo',
      closure_size: 2048,
      prefer_local_build: true,
      is_fixed_output: false,
      estimate: {
        own_samples: 4,
        cores: 43,
        ram_mb: 777,
        build_ms: 4200,
        output_nar_bytes: null,
        similar_builds: [['python3.14-torchaudio', 0.75], ['magma', 0.4]],
      },
      derivations: [{ build_id: 'b1', drv_path: '/nix/store/xxx-foo', pname: 'foo' }],
    },
    instance_context: {
      wait_secs: { w5m: 1, w1h: 2, w24h: 3 },
      build_time_ms: { w5m: 100, w1h: 200, w24h: 300 },
      peak_ram_mb: { w5m: 10, w1h: 20, w24h: 30 },
      cpu_time_ms: { w5m: 1, w1h: 2, w24h: 3 },
      avg_cpu_pct: { w5m: 1, w1h: 2, w24h: 3 },
      disk_bytes: { w5m: 1, w1h: 2, w24h: 3 },
      oom_rate: { w5m: 0, w1h: 0, w24h: 0 },
      closure_size: { w5m: 1, w1h: 2, w24h: 3 },
      nar_size_mb: { w5m: 1, w1h: 2, w24h: 3 },
      missing_paths: { w5m: 1, w1h: 2, w24h: 3 },
      dependency_cnt: { w5m: 1, w1h: 2, w24h: 3 },
      completed: { w5m: 1, w1h: 2, w24h: 3 },
      active_builds: 7,
      pending_builds: 4,
      total_workers: 5,
      idle_workers: 2,
    },
    candidates: null,
  },
  passed_over: false,
  previous_attempts: [],
};

const EVAL_DETAIL: AssignedJobDetail = {
  ...DETAIL,
  kind: 0,
  derivations: [],
  evaluation: {
    repository: 'https://git.example/acme.git',
    commit: 'abc1234def5678abc1234def5678abc1234def56',
    commit_message: 'Bump inputs',
    wildcard: 'packages.**',
    status: 'Building',
    task: 'ci',
  },
  assigned_scheduler_reasoning: {
    ...DETAIL.assigned_scheduler_reasoning!,
    job_context: {
      kind: 'Eval',
      architecture: '',
      missing_count: null,
      missing_nar_size: null,
      outputs_present: false,
      project_work_share: null,
      rescore_count: 0,
      queued_at: '2026-06-08T00:00:00Z',
      ready_at: '2026-06-08T00:00:30Z',
      fetch_flake: true,
    },
  },
};

const PENDING: PendingJobSummary = {
  kind: 1,
  project: 'o1',
  evaluation_id: 'e1',
  build_id: 'b9',
  queued_at: '2026-06-08T00:00:00Z',
  dependency_count: 3,
  subject: 'hello-2.12.1',
  permissions: [],
};

function setup(board: Partial<BoardService> = {}, id = 'job-1', pending = false): ComponentFixture<BoardJobDetailComponent> {
  TestBed.configureTestingModule({
    imports: [BoardJobDetailComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ id }), data: { pending } },
          paramMap: of(convertToParamMap({ id })),
        },
      },
      {
        provide: BoardService,
        useValue: {
          getJob: () => of(DETAIL),
          getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
          getLatestAssignmentForEvaluation: () => throwError(() => ({ status: 404 })),
          getScoringRules: () => of([]),
          ...board,
        },
      },
      { provide: EvaluationsService, useValue: { getBuild: () => EMPTY } },
      { provide: ExecJobsService, useValue: { abort: () => EMPTY } },
      { provide: AuthService, useValue: { user: () => ({ superuser: true }) } },
    ],
  });
  const fixture = TestBed.createComponent(BoardJobDetailComponent);
  fixture.detectChanges();
  return fixture;
}

function noJsonDump(el: HTMLElement): void {
  expect(el.querySelectorAll('pre').length).toBe(0);
}

describe('BoardJobDetailComponent - run time', () => {
  it('shows one queue time, the wait and how long the worker ran', () => {
    const finished = { ...DETAIL, finished_at: '2026-06-08T00:01:10Z', worker_elapsed_ms: 9000, worker_tail_ms: 500, transit_ms: 1000 };
    const el = setup({ getJob: () => of(finished) }).nativeElement as HTMLElement;
    const steps = Array.from(el.querySelectorAll('section.timeline .step'));
    expect(steps.map((s) => s.querySelector('.label')?.textContent?.trim())).toEqual(['Queued', 'Wait', 'Started', 'Worker time']);
    expect(steps[3].lastElementChild?.textContent?.trim()).toBe('9.0 s');
  });

  it('leaves out the run time for a row the server closed without a worker report', () => {
    const closed = { ...DETAIL, finished_at: '2026-06-08T00:05:00Z' };
    const steps = (setup({ getJob: () => of(closed) }).nativeElement as HTMLElement).querySelector('section.timeline')?.textContent ?? '';
    expect(steps).not.toContain('Worker time');
    expect(steps).not.toContain('Running for');
  });

  it('shows the start and a live elapsed time while the job runs', () => {
    vi.useFakeTimers({ now: new Date('2026-06-08T00:03:00Z') });
    try {
      const el = setup().nativeElement as HTMLElement;
      const steps = Array.from(el.querySelectorAll('section.timeline .step'));
      expect(steps.map((s) => s.querySelector('.label')?.textContent?.trim())).toEqual(['Queued', 'Wait', 'Started', 'Running for']);
      expect(steps[3].lastElementChild?.textContent?.trim()).toBe(formatDuration(120_000));
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('BoardJobDetailComponent - structured context panels', () => {
  it('renders the worker cpu_count under a Worker-context section', () => {
    const el = setup().nativeElement as HTMLElement;
    noJsonDump(el);
    const worker = el.querySelector('section.ctx.worker') as HTMLElement;
    expect(worker).toBeTruthy();
    expect(worker.textContent).toContain('Worker context');
    expect(worker.textContent).toContain('8');
  });

  it('lists only worker capabilities, never the server-only core/cache flags', () => {
    const el = setup().nativeElement as HTMLElement;
    const caps = Array.from(el.querySelectorAll('.ctx.worker tr')).find((r) =>
      r.textContent?.includes('Capabilities'),
    ) as HTMLElement;
    expect(caps.textContent).toContain('network');
    expect(caps.textContent).toContain('build');
    expect(caps.textContent).not.toContain('core');
    expect(caps.textContent).not.toContain('cache');
  });

  it('has no redundant standalone Fetch row in the worker context', () => {
    const el = setup().nativeElement as HTMLElement;
    const labels = Array.from(el.querySelectorAll('.ctx.worker td.label')).map((c) => c.textContent?.trim());
    expect(labels).not.toContain('Fetch');
  });

  it('renders a derivations row with the pname / drv_path', () => {
    const el = setup().nativeElement as HTMLElement;
    const drv = el.querySelector('.drv-row') as HTMLElement;
    expect(drv).toBeTruthy();
    expect(drv.textContent).toContain('foo');
    expect(drv.textContent).toContain('/nix/store/xxx-foo');
  });

  it('opens the build popup on the per-eval build id, not the shared build', () => {
    const ids: string[] = [];
    TestBed.overrideProvider(EvaluationsService, {
      useValue: { getBuild: (id: string) => { ids.push(id); return EMPTY; } },
    });
    const fixture = setup();
    const drv = fixture.nativeElement.querySelector('.drv-row') as HTMLElement;
    drv.click();
    expect(ids).toEqual(['bj1']);
  });

  it('leaves a derivation whose build the eval no longer has unclickable', () => {
    const detail = {
      ...DETAIL,
      derivations: [{ build: null, derivation_build: 'a1', drv_path: '/nix/store/xxx-foo', pname: 'foo' }],
    };
    const el = setup({ getJob: () => of(detail) }).nativeElement as HTMLElement;
    expect(el.querySelector('.ctx.job .drv-row.clickable')).toBeNull();
  });

  it('links the worker id to the project-scoped worker metrics page', () => {
    const el = setup().nativeElement as HTMLElement;
    const link = el.querySelector('.ids .worker-link') as HTMLAnchorElement;
    expect(link).toBeTruthy();
    expect(link.getAttribute('href')).toBe('/project/project-one/workers/w1/metrics');
  });

  it('names the worker without a link for a viewer outside the project', () => {
    const el = setup({ getJob: () => of({ ...DETAIL, can_open_worker_metrics_page: false }) }).nativeElement as HTMLElement;
    expect(el.querySelector('.ids .worker-link')).toBeNull();
    expect(el.querySelector('.ids .worker-name')?.textContent?.trim()).toBe('Builder One');
  });

  it('names the worker and the project by their display names', () => {
    const el = setup().nativeElement as HTMLElement;
    const project = el.querySelector('.ids .project-link') as HTMLElement;
    expect(project.textContent?.trim()).toBe('Project One');
    expect(project.getAttribute('href')).toBe('/project/project-one');
    expect(el.querySelector('.ids .worker-link')?.textContent?.trim()).toBe('Builder One');
  });

  it('labels the log link as the log, leaving "Evaluation" to the section heading', () => {
    const el = setup({ getJob: () => of(EVAL_DETAIL) }).nativeElement as HTMLElement;
    const labels = Array.from(el.querySelectorAll('.ids .label')).map((l) => l.textContent?.trim());
    expect(labels).toContain('Log');
    expect(labels).not.toContain('Evaluation');
    expect(el.querySelector('.ids .log-link')?.textContent?.trim()).toBe('Open log');
  });

  it('shows plain text instead of a dead log link once the evaluation is deleted', () => {
    const el = setup().nativeElement as HTMLElement;
    expect(el.querySelector('.ids .log-link')).toBeNull();
    expect(el.querySelector('.ids')?.textContent).toContain('Evaluation deleted');
  });

  it('falls back to the project name and marks an unnamed worker as not shared, without a link', () => {
    const detail = { ...DETAIL, worker_name: null, project_display_name: '' };
    const el = setup({ getJob: () => of(detail) }).nativeElement as HTMLElement;
    expect(el.querySelector('.ids .project-link')?.textContent?.trim()).toBe('project-one');
    expect(el.querySelector('.ids .worker-link')).toBeNull();
    expect(el.querySelector('.ids')?.textContent).toContain('Not shared with this account');
    expect(el.querySelector('.ids')?.textContent).not.toContain('w1');
  });

  it('leaves out the scheduler details when the server sends no scheduler reasoning', () => {
    TestBed.overrideProvider(AuthService, { useValue: { user: () => ({ superuser: false }) } });
    const el = setup({ getJob: () => of({ ...DETAIL, assigned_scheduler_reasoning: null }) }).nativeElement as HTMLElement;
    expect(el.querySelector('details.scheduler')).toBeNull();
    expect(el.textContent).not.toContain('Score breakdown');
    expect(el.querySelector('section.timeline')).toBeTruthy();
  });

  it('shows the architecture for build jobs', () => {
    const el = setup().nativeElement as HTMLElement;
    const job = el.querySelector('section.ctx.job') as HTMLElement;
    expect(job.textContent).toContain('Architecture');
  });

  it('shows the chosen cores and the builds the estimate is taken from', () => {
    const el = setup().nativeElement as HTMLElement;
    const text = el.textContent ?? '';
    expect(text).toContain('Estimate');
    expect(text).toContain('43');
    expect(text).toContain('Like python3.14-torchaudio');
    expect(text).toContain('75% same inputs');
    expect(text).toContain('40% same inputs');
  });

  it('keeps the scheduler internals in one closed section, without repeating the timeline dates', () => {
    const el = setup().nativeElement as HTMLElement;
    const details = el.querySelector('details.scheduler') as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(details.querySelectorAll('section.ctx').length).toBe(3);
    const jobLabels = Array.from(details.querySelectorAll('section.ctx.job td.label')).map((td) => td.textContent?.trim());
    expect(jobLabels).not.toContain('Queued');
    expect(jobLabels).not.toContain('Ready');
    expect(el.querySelector('.head')?.textContent).not.toContain('score');
  });

  it('hides the architecture for eval jobs', () => {
    const el = setup({ getJob: () => of(EVAL_DETAIL) }).nativeElement as HTMLElement;
    const job = el.querySelector('section.ctx.job') as HTMLElement;
    expect(job.textContent).toContain('Eval');
    expect(job.textContent).not.toContain('Architecture');
  });
});

describe('BoardJobDetailComponent - back link', () => {
  it('leads an assigned job back to the live jobs list', () => {
    const back = (setup().nativeElement as HTMLElement).querySelector('a.back');
    expect(back?.textContent).toContain('Live Jobs');
    expect(back?.getAttribute('href')).toBe('/board/live');
  });
});

describe('BoardJobDetailComponent - pending route', () => {
  it('renders a limited pending view from the pending list without asking for an assigned job', () => {
    const getJob = vi.fn(() => of(DETAIL));
    const el = setup({ getJob, getPendingJobs: () => of({ jobs: [PENDING], other_pending: 0 }) }, 'b9', true)
      .nativeElement as HTMLElement;
    expect(getJob).not.toHaveBeenCalled();
    expect(el.querySelector('h1')?.textContent).toBe('hello-2.12.1');
    const back = el.querySelector('a.back');
    expect(back?.textContent).toContain('Pending jobs');
    expect(back?.getAttribute('href')).toBe('/board/live?view=pending');
    expect(el.textContent).not.toContain('Score breakdown');
    expect(el.querySelector('section.timeline')?.textContent).toContain('Dependencies');
  });

  it('leaves out the dependency step when nothing has to build first', () => {
    const el = setup({ getPendingJobs: () => of({ jobs: [{ ...PENDING, dependency_count: 0 }], other_pending: 0 }) }, 'b9', true)
      .nativeElement as HTMLElement;
    expect(el.querySelector('section.timeline')?.textContent).not.toContain('Dependencies');
  });

  it('reports an assigned job id the server does not know as missing, without a retry', () => {
    const getPendingJobs = vi.fn(() => of({ jobs: [PENDING], other_pending: 0 }));
    const el = setup({ getJob: () => throwError(() => ({ status: 404 })), getPendingJobs }, 'e1')
      .nativeElement as HTMLElement;
    expect(getPendingJobs).not.toHaveBeenCalled();
    expect(el.textContent).toContain('Not found or access denied');
    expect(el.textContent).not.toContain('Try Again');
  });

  it('reports a server failure as an error with a retry instead of a missing job', () => {
    const getJob = vi.fn((): Observable<AssignedJobDetail> => throwError(() => ({ status: 500, message: 'database unavailable' })));
    const fixture = setup({ getJob });
    const el = fixture.nativeElement as HTMLElement;
    expect(el.textContent).toContain("Couldn't load job");
    expect(el.textContent).toContain('database unavailable');

    getJob.mockReturnValue(of(DETAIL));
    (el.querySelector('gr-load-state button') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(el.querySelector('section.ids')).toBeTruthy();
  });

  it('explains why a pending evaluation waits and links its project, task and evaluation', () => {
    const pending: PendingJobSummary = {
      ...PENDING,
      kind: 0,
      build_id: null,
      project_name: 'sandbox',
      task: 'oktask',
      waiting_reason: { kind: 'no_cache' },
      permissions: ['manageSubscriptions'],
    };
    const el = setup({ getPendingJobs: () => of({ jobs: [pending], other_pending: 0 }) }, 'e1', true)
      .nativeElement as HTMLElement;

    expect(el.querySelector('gr-waiting-notice')?.textContent).toContain('No writable cache');
    const hrefs = Array.from(el.querySelectorAll('section.ids a, gr-waiting-notice a')).map((a) => a.getAttribute('href'));
    expect(hrefs).toEqual(expect.arrayContaining([
      '/project/sandbox',
      '/project/sandbox/task/oktask?eval=e1',
      '/project/sandbox/log/e1',
      '/project/sandbox/caches',
    ]));
  });

  it('names an id that is neither pending nor running as not found, with a heading', () => {
    const el = setup({ getPendingJobs: () => of({ jobs: [], other_pending: 0 }) }, 'missing', true)
      .nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent).toBe('Job not found');
    expect(el.textContent).not.toContain('Not found or access denied');
  });

  it('names a malformed pending id the server rejects as not found, without a retry', () => {
    const el = setup({
      getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
      getLatestAssignmentForEvaluation: () => throwError(() => ({ status: 400, message: 'Invalid URL: UUID parsing failed' })),
    }, 'not-a-job-id', true).nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent).toBe('Job not found');
    expect(el.textContent).not.toContain('Try Again');
  });

  it('opens the latest assignment when a worker has already taken the job', () => {
    const getLatestAssignmentForEvaluation = vi.fn(() => of({ dispatched_job_id: 'job-7', evaluation_id: 'e1', project_name: 'sandbox' }));
    const fixture = setup({ getPendingJobs: () => of({ jobs: [], other_pending: 0 }), getLatestAssignmentForEvaluation }, 'b9', true);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl');
    fixture.componentInstance.reload();
    expect(getLatestAssignmentForEvaluation).toHaveBeenCalledWith('b9');
    expect(String(navigate.mock.calls[0]?.[0])).toBe('/board/jobs/job-7');
    expect(navigate.mock.calls[0]?.[1]).toEqual({ replaceUrl: true });
  });

  it('links the evaluation of a job that left the queue without an assignment', () => {
    const el = setup({
      getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
      getLatestAssignmentForEvaluation: () => of({ dispatched_job_id: null, evaluation_id: 'e1', project_name: 'sandbox' }),
    }, 'e1', true).nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent).toBe('Job not found');
    expect(el.querySelector('a.open-evaluation')?.getAttribute('href')).toBe('/project/sandbox/log/e1');
  });

  it('shows the commit of a pending evaluation job', () => {
    const pending: PendingJobSummary = { ...PENDING, kind: 0, build_id: null, subject: 'https://example.com/r.git', commit: 'abcdef1234567', commit_message: 'Fix the build' };
    const el = setup({ getPendingJobs: () => of({ jobs: [pending], other_pending: 0 }) }, 'e1', true).nativeElement as HTMLElement;
    expect(el.querySelector('section.ids .commit')?.textContent).toBe('abcdef12 Fix the build');
  });

  it('does not match a pending build of the same evaluation for an evaluation id', () => {
    const el = setup({ getPendingJobs: () => of({ jobs: [PENDING], other_pending: 0 }) }, 'e1', true)
      .nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent).toBe('Job not found');
  });
});

describe('BoardJobDetailComponent - pending refresh', () => {
  afterEach(() => vi.useRealTimers());

  it('keeps asking while the job waits and marks it once it left the queue', () => {
    vi.useFakeTimers();
    const waiting: PendingJobSummary = { ...PENDING, project_name: 'sandbox', waiting_reason: { kind: 'no_cache' } };
    const getPendingJobs = vi.fn(() => of({ jobs: [waiting], other_pending: 0 }));
    const fixture = setup({ getPendingJobs }, 'b9', true);
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('gr-waiting-notice')).toBeTruthy();

    vi.advanceTimersByTime(5_000);
    expect(getPendingJobs).toHaveBeenCalledTimes(2);

    getPendingJobs.mockReturnValue(of({ jobs: [], other_pending: 0 }));
    vi.advanceTimersByTime(5_000);
    fixture.detectChanges();
    expect(el.querySelector('.head .left-queue')?.textContent?.trim()).toBe('Left the queue');
    expect(el.querySelector('gr-waiting-notice')).toBeNull();
    expect(el.querySelector('h1')?.textContent).toBe('hello-2.12.1');
    expect(Array.from(el.querySelectorAll('section.ids a')).map((a) => a.getAttribute('href'))).toContain('/project/sandbox/log/e1');

    vi.advanceTimersByTime(15_000);
    expect(getPendingJobs).toHaveBeenCalledTimes(3);
  });
});

describe('BoardJobDetailComponent - previous build attempts', () => {
  const WITH_ATTEMPTS: AssignedJobDetail = {
    ...DETAIL,
    previous_attempts: [
      { dispatched_job_id: 'dj-a1', substitute: false, outcome: 3, reason: 5, failure_message: 'builder for ... failed with exit code 1', created_at: '2026-06-08T00:00:00Z' },
      { dispatched_job_id: 'dj-a2', substitute: true,  outcome: 2, reason: null, failure_message: null, created_at: '2026-06-08T00:01:00Z' },
    ],
  };

  it('renders the Build attempts section when there are multiple attempts', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    expect(el.textContent).toContain('Build attempts');
  });

  it('renders one row per attempt', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.attempts') as HTMLElement;
    const rows = section.querySelectorAll('tbody tr');
    expect(rows.length).toBe(2);
  });

  it('strips terminal colour codes from the failure reason', () => {
    const coloured: AssignedJobDetail = {
      ...WITH_ATTEMPTS,
      previous_attempts: [
        { ...WITH_ATTEMPTS.previous_attempts[0], failure_message: '\u001b[35;1m/nix/store/foo.drv\u001b[0m failed' },
        WITH_ATTEMPTS.previous_attempts[1],
      ],
    };
    const el = setup({ getJob: () => of(coloured) }).nativeElement as HTMLElement;
    expect(el.querySelector('section.attempts td.reason')?.textContent).toBe('/nix/store/foo.drv failed');
  });

  // An assignment the worker never reported back on used to read as 'running'
  // forever, on a worker that had long since left the fleet.
  it('reports an abandoned assignment as aborted, not running', () => {
    const abandoned: AssignedJobDetail = {
      ...DETAIL,
      finished_at: '2026-06-08T00:31:00Z',
      outcome: 'abandoned',
    };
    const el = setup({ getJob: () => of(abandoned) }).nativeElement as HTMLElement;
    expect(el.querySelector('.head gr-status-badge')?.textContent?.trim()).toBe('Aborted');
  });

  it('still reports an unfinished assignment as running', () => {
    const el = setup({ getJob: () => of(DETAIL) }).nativeElement as HTMLElement;
    expect(el.querySelector('.head gr-status-badge')?.textContent?.trim()).toBe('Running');
  });

  it('shows mode and outcome labels for each attempt', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.attempts') as HTMLElement;
    expect(section.textContent).toContain('build');
    expect(section.textContent).toContain('failed');
    expect(section.textContent).toContain('substitute');
    expect(section.textContent).toContain('substituted');
  });

  it('each row links to the assigned job', () => {
    const fixture = setup({ getJob: () => of(WITH_ATTEMPTS) });
    const links = fixture.debugElement
      .query(By.css('section.attempts'))
      .queryAll(By.directive(RouterLink));
    const urls = links.map((l) => l.injector.get(RouterLink).urlTree?.toString());
    expect(urls).toContain('/board/jobs/dj-a1');
    expect(urls).toContain('/board/jobs/dj-a2');
  });

  it('leaves an attempt without a dispatched job unlinked', () => {
    const undispatched: AssignedJobDetail = {
      ...WITH_ATTEMPTS,
      previous_attempts: [
        ...WITH_ATTEMPTS.previous_attempts,
        { dispatched_job_id: null, substitute: false, outcome: 3, reason: 10, failure_message: 'no connected worker has enough memory', created_at: '2026-06-08T00:02:00Z' },
      ],
    };
    const fixture = setup({ getJob: () => of(undispatched) });
    const section = fixture.debugElement.query(By.css('section.attempts'));
    const rows = section.queryAll(By.css('tbody tr'));
    const urls = section
      .queryAll(By.directive(RouterLink))
      .map((l) => l.injector.get(RouterLink).urlTree?.toString());

    expect(rows.length).toBe(3);
    expect(rows[2].nativeElement.classList).not.toContain('clickable');
    expect(urls).not.toContain('/board/jobs/null');
  });

  it('hides the section when there is only one attempt', () => {
    const singleAttempt: AssignedJobDetail = {
      ...DETAIL,
      previous_attempts: [
        { dispatched_job_id: 'dj-a1', substitute: false, outcome: 1, reason: null, failure_message: null, created_at: '2026-06-08T00:00:00Z' },
      ],
    };
    const el = setup({ getJob: () => of(singleAttempt) }).nativeElement as HTMLElement;
    expect(el.textContent).not.toContain('Build attempts');
  });

  it('shows the failure message in the reason column', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.attempts') as HTMLElement;
    expect(section.textContent).toContain('builder for ... failed with exit code 1');
  });
});

describe('BoardJobDetailComponent - exec jobs', () => {
  const EXEC_JOB: BoardExecJob = {
    id: 'x1',
    project: 'project-one',
    task: 'nightly',
    task_display_name: 'Nightly',
    entry_point: 'apps.x86_64-linux.train',
    node_index: 1,
    nodes: 2,
  };
  const EXEC_DETAIL: AssignedJobDetail = {
    ...EVAL_DETAIL,
    kind: 2,
    evaluation: null,
    finished_at: '2026-06-08T00:05:00Z',
    outcome: 'completed',
    exec_job: EXEC_JOB,
    exec_attempt: {
      number: 2,
      worker: 'w1',
      started_at: '2026-06-08T00:01:30',
      ended_at: '2026-06-08T00:05:00',
      end_reason: 'completed',
      message: null,
    },
    assigned_scheduler_reasoning: {
      ...EVAL_DETAIL.assigned_scheduler_reasoning!,
      score: 3.75,
      job_context: { ...EVAL_DETAIL.assigned_scheduler_reasoning!.job_context, kind: 'Exec', architecture: 'x86_64-linux', missing_count: 4 },
    },
  };

  it('names the kind, its score and the exec job it ran with a link to its page', () => {
    const fixture = setup({ getJob: () => of(EXEC_DETAIL) });
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.head gr-badge.kind')?.textContent?.trim()).toBe('exec');
    expect(el.querySelector('details.scheduler tr.total .num')?.textContent?.trim()).toBe('3.8');
    const section = el.querySelector('section.exec') as HTMLElement;
    expect(section.textContent).toContain('nightly');
    expect(section.textContent).toContain('1 of 2 nodes');
    expect(section.textContent).toContain('Completed');
    const link = fixture.debugElement.query(By.css('section.exec a.exec-link'));
    expect(link.injector.get(RouterLink).urlTree?.toString()).toBe('/project/project-one/task/nightly/exec/x1?node=1');
  });

  it('links the log of the evaluation the exec job runs in, without the evaluation table', () => {
    const live = { ...EXEC_DETAIL, evaluation: EVAL_DETAIL.evaluation };
    const fixture = setup({ getJob: () => of(live) });
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.ids')?.textContent).not.toContain('Evaluation deleted');
    const link = fixture.debugElement.query(By.css('section.ids a.log-link'));
    expect(link.injector.get(RouterLink).urlTree?.toString()).toBe('/project/project-one/log/e1');
    expect(el.querySelector('section.evaluation')).toBeNull();
  });

  it('shows the queue time and the wait without repeating the same timestamp', () => {
    const el = setup({ getJob: () => of(EXEC_DETAIL) }).nativeElement as HTMLElement;
    const labels = Array.from(el.querySelectorAll('section.timeline .step .label')).map((s) => s.textContent?.trim());
    expect(labels.slice(0, 2)).toEqual(['Queued', 'Wait']);
    expect(labels).not.toContain('Assigned');
    expect(el.querySelector('.head gr-status-badge')?.textContent?.trim()).toBe('Completed');
  });

  it('names the finish once and leaves out an empty worker timeline', () => {
    const el = setup({ getJob: () => of({ ...EXEC_DETAIL, phases: [] }) }).nativeElement as HTMLElement;
    const execLabels = Array.from(el.querySelectorAll('section.exec td.label')).map((td) => td.textContent?.trim());
    expect(execLabels).not.toContain('Ended');
    expect(el.querySelector('gr-job-timeline')).toBeNull();
  });

  it('shows why a failed attempt ended next to its end reason', () => {
    const failed: AssignedJobDetail = { ...EXEC_DETAIL, exec_attempt: { ...EXEC_DETAIL.exec_attempt!, end_reason: 'failed', message: 'program exited with code 3' } };
    const cell = (setup({ getJob: () => of(failed) }).nativeElement as HTMLElement).querySelector('section.exec .end-reason');
    expect(cell?.textContent?.replace(/\s+/g, ' ').trim()).toBe('Failed program exited with code 3');
  });

  it('labels a timed-out attempt and drops a message that only repeats the reason', () => {
    const timedOut: AssignedJobDetail = { ...EXEC_DETAIL, exec_attempt: { ...EXEC_DETAIL.exec_attempt!, end_reason: 'timeout', message: 'timed out' } };
    const cell = (setup({ getJob: () => of(timedOut) }).nativeElement as HTMLElement).querySelector('section.exec .end-reason');
    expect(cell?.textContent?.trim()).toBe('Timed out');
  });

  it('shows the architecture of the exec job but no flake fetch', () => {
    const job = (setup({ getJob: () => of(EXEC_DETAIL) }).nativeElement as HTMLElement).querySelector('section.ctx.job') as HTMLElement;
    expect(job.textContent).toContain('Architecture');
    expect(job.textContent).not.toContain('Fetch flake');
  });

  function fakeConfirm(confirmed = true) {
    const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmed ? request.action() : EMPTY));
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    return confirm;
  }

  it('aborts the exec job, not its evaluation, after naming it', () => {
    const aborted: string[] = [];
    const confirm = fakeConfirm();
    TestBed.overrideProvider(ExecJobsService, { useValue: { abort: (id: string) => { aborted.push(id); return of('Success'); } } });
    TestBed.overrideProvider(AuthService, { useValue: { user: () => ({ superuser: true }) } });
    const fixture = setup({ getJob: () => of({ ...EXEC_DETAIL, finished_at: null, outcome: null }) });
    const cmp = fixture.componentInstance;
    expect(cmp.canAbort()).toBe(true);
    cmp.abort();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Abort Exec Job', target: 'nightly: apps.x86_64-linux.train (node 1)' }));
    expect(aborted).toEqual(['x1']);
  });

  it('confirms a finished abort with a toast named after the job kind', () => {
    fakeConfirm();
    const fixture = setup({ getJob: () => of(DETAIL), abortJob: () => of('Success') });
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    const cmp = fixture.componentInstance;
    cmp.abort();
    expect(success).toHaveBeenLastCalledWith('Build aborted');
    cmp.job.set({ ...EVAL_DETAIL, id: 'job-2' });
    cmp.abort();
    expect(success).toHaveBeenLastCalledWith('Evaluation aborted');
  });

  it('aborts a running build or eval job on the board, naming the package and task or the task and commit', () => {
    const aborted: string[] = [];
    const confirm = fakeConfirm();
    TestBed.overrideProvider(AuthService, { useValue: { user: () => ({ superuser: true }) } });
    const abortJob = (id: string) => { aborted.push(id); return of('Success'); };
    const build: AssignedJobDetail = { ...DETAIL, build_id: null, evaluation: { ...EVAL_DETAIL.evaluation!, task_display_name: 'Continuous Integration' } };
    const fixture = setup({ getJob: () => of(build), abortJob });
    const cmp = fixture.componentInstance;
    expect(cmp.canAbort()).toBe(true);
    cmp.abort();
    expect(confirm).toHaveBeenLastCalledWith(expect.objectContaining({ title: 'Abort Build', target: 'foo · Continuous Integration' }));
    cmp.job.set({ ...EVAL_DETAIL, id: 'job-2', finished_at: null });
    cmp.abort();
    expect(confirm).toHaveBeenLastCalledWith(expect.objectContaining({ title: 'Abort Evaluation', target: 'ci · abc1234d' }));
    expect(aborted).toEqual(['job-1', 'job-2']);
  });

  it('sends nothing when the abort is cancelled', () => {
    const abortJob = vi.fn(() => of('Success'));
    fakeConfirm(false);
    TestBed.overrideProvider(AuthService, { useValue: { user: () => ({ superuser: true }) } });
    const fixture = setup({ abortJob });
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    fixture.componentInstance.abort();
    expect(abortJob).not.toHaveBeenCalled();
    expect(success).not.toHaveBeenCalled();
  });

  it('finds a pending exec job by its exec job id', () => {
    const pending: PendingJobSummary = { ...PENDING, kind: 2, build_id: null, subject: EXEC_JOB.entry_point, exec_job: EXEC_JOB };
    const el = setup({ getPendingJobs: () => of({ jobs: [PENDING, pending], other_pending: 0 }) }, 'x1', true)
      .nativeElement as HTMLElement;
    expect(el.querySelector('.head .kind')?.textContent?.trim()).toBe('exec');
    expect(el.querySelector('.exec-link')?.textContent?.trim()).toBe('apps.x86_64-linux.train (node 1)');
  });
});

describe('BoardJobDetailComponent - live state', () => {
  afterEach(() => vi.useRealTimers());

  it('follows a running job until it ends and then drops Abort and stops asking', () => {
    vi.useFakeTimers();
    const finished = { ...DETAIL, finished_at: '2026-06-08T00:05:00Z', outcome: 'abandoned' };
    const getJob = vi.fn(() => of(DETAIL));
    const fixture = setup({ getJob });
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.top-row button')?.textContent).toContain('Abort');
    expect(el.querySelector('.top-row button .gr-button__icon')?.textContent?.trim()).toBe('stop_circle');

    getJob.mockReturnValue(of(finished));
    vi.advanceTimersByTime(5_000);
    fixture.detectChanges();
    expect(el.querySelector('.top-row button')).toBeNull();
    expect(el.querySelector('.head gr-status-badge')?.textContent?.trim()).toBe('Aborted');

    vi.advanceTimersByTime(15_000);
    expect(getJob).toHaveBeenCalledTimes(2);
  });
});

describe('BoardJobDetailComponent - #636', () => {
  it('shows an eval job the evaluation it ran, not an empty derivation list', () => {
    const el = setup({ getJob: () => of(EVAL_DETAIL) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.evaluation') as HTMLElement;
    expect(section.textContent).toContain('https://git.example/acme.git');
    expect(section.textContent).toContain('abc1234');
    expect(section.textContent).toContain('Bump inputs');
    expect(section.textContent).toContain('packages.**');
    expect(el.textContent).not.toContain('Derivations');
  });

  it('titles a build job by its package with the outcome next to it, and names its task', () => {
    const job: AssignedJobDetail = {
      ...DETAIL,
      finished_at: '2026-06-08T00:05:00Z',
      outcome: 'failed',
      evaluation: { repository: 'https://example.com/r.git', commit: 'abc1234', wildcard: 'packages.**', status: 'Failed', task: 'nightly' },
    };
    const el = setup({ getJob: () => of(job) }).nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent?.trim()).toBe('foo');
    expect(el.querySelector('.head gr-status-badge')?.textContent?.trim()).toBe('Failed');
    expect(el.querySelector('a.task-link')?.textContent?.trim()).toBe('nightly');
    expect(TestBed.inject(Title).getTitle()).toBe('foo · Job Board · Gradient');
  });

  it('titles an evaluation job by its task', () => {
    const el = setup({ getJob: () => of(EVAL_DETAIL) }).nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent?.trim()).toBe('ci');
    expect(el.querySelector('.head gr-status-badge')?.textContent?.trim()).toBe('Running');
  });

  it('links the evaluation to its log page', () => {
    const fixture = setup({ getJob: () => of(EVAL_DETAIL) });
    const link = fixture.debugElement.query(By.css('section.ids a[href*="/log/"]'));
    expect(link.injector.get(RouterLink).urlTree?.toString()).toBe('/project/project-one/log/e1');
  });

  it('marks the attempt being viewed instead of linking it to itself', () => {
    const job: AssignedJobDetail = {
      ...DETAIL,
      previous_attempts: [
        { dispatched_job_id: 'dj-old', substitute: false, outcome: 3, reason: null, failure_message: null, created_at: '2026-06-08T00:00:00Z' },
        { dispatched_job_id: 'job-1', substitute: false, outcome: 1, reason: null, failure_message: null, created_at: '2026-06-08T00:01:00Z' },
      ],
    };
    const el = setup({ getJob: () => of(job) }).nativeElement as HTMLElement;
    const rows = el.querySelectorAll('section.attempts tbody tr');
    expect(rows[1].classList).toContain('current');
    expect(rows[1].classList).not.toContain('clickable');
    expect(rows[1].textContent).not.toContain('\u203a');
    expect(rows[0].classList).toContain('clickable');
  });

  it('reads sizes and times in units a person can take in', () => {
    const text = (setup().nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('31.3 GiB');
    expect(text).toContain('1.0 KiB');
    expect(text).toContain('4.2 s');
    expect(text).toContain('777 MiB');
  });

  it('formats the fleet averages by unit and leaves out metrics nobody reported', () => {
    const reasoning = DETAIL.assigned_scheduler_reasoning!;
    const job: AssignedJobDetail = {
      ...DETAIL,
      assigned_scheduler_reasoning: {
        ...reasoning,
        instance_context: {
          ...reasoning.instance_context!,
          closure_size: { w5m: null, w1h: 6_503_595.33, w24h: null },
          disk_bytes: { w5m: null, w1h: null, w24h: null },
        },
      },
    };
    const instance = (setup({ getJob: () => of(job) }).nativeElement as HTMLElement).querySelector('section.ctx.instance') as HTMLElement;
    const closure = Array.from(instance.querySelectorAll('tr')).find((r) => r.textContent?.includes('Closure size'));
    expect(closure?.textContent).toContain('6.2 MiB');
    expect(instance.textContent).not.toContain('6,503,595');
    expect(instance.textContent).not.toContain('Disk usage');
  });

  it('names each rule help button after its rule', () => {
    const reasoning = DETAIL.assigned_scheduler_reasoning!;
    const job = { ...DETAIL, assigned_scheduler_reasoning: { ...reasoning, score_breakdown: { rules: { WaitTimeRule: 2, QosRule: 1 }, total: 3 } } };
    TestBed.overrideProvider(BoardService, {
      useValue: {
        getJob: () => of(job),
        getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
        getScoringRules: () => of([{ rule: 'WaitTimeRule', description: 'w' }, { rule: 'QosRule', description: 'q' }]),
      },
    });
    const el = setup().nativeElement as HTMLElement;
    const names = Array.from(el.querySelectorAll('button.help')).map((b) => b.getAttribute('aria-label'));
    expect(names).toEqual(['Explain Wait time', 'Explain Service quality']);
  });

  it('names the pending job by what it works on', () => {
    const el = setup({ getPendingJobs: () => of({ jobs: [PENDING], other_pending: 0 }) }, 'b9', true).nativeElement as HTMLElement;
    expect(el.textContent).toContain('hello-2.12.1');
  });
});

describe('getPendingJobName', () => {
  it('names a pending job like the page of the assigned job', () => {
    const exec = { id: 'x1', project: 'o1', entry_point: 'deploy', task: 'nightly', task_display_name: 'Nightly', node_index: 0, nodes: 1 };
    expect(getPendingJobName({ ...PENDING, kind: 2, exec_job: exec })).toBe('nightly: deploy');
    expect(getPendingJobName(PENDING)).toBe('hello-2.12.1');
    expect(getPendingJobName({ ...PENDING, kind: 0, task: 'oktask', project_name: 'sandbox' })).toBe('oktask');
  });

  it('prefers the task display name over its slug', () => {
    const exec = { id: 'x1', project: 'o1', entry_point: 'deploy', task: 'nightly', task_display_name: 'Nightly Deploy', node_index: 0, nodes: 1 };
    expect(getPendingJobName({ ...PENDING, kind: 2, exec_job: exec, task: 'nightly', task_display_name: 'Nightly Deploy' })).toBe('Nightly Deploy: deploy');
    expect(getPendingJobName({ ...PENDING, kind: 0, task: 'oktask', task_display_name: 'OK Task' })).toBe('OK Task');
  });
});

describe('BoardJobDetailComponent - display names', () => {
  it('names the project and task of a pending job by their display names', () => {
    const pending: PendingJobSummary = {
      ...PENDING,
      kind: 0,
      build_id: null,
      project_name: 'sandbox',
      project_display_name: 'Sandbox',
      task: 'execstates',
      task_display_name: 'Exec States Task',
    };
    const el = setup({ getPendingJobs: () => of({ jobs: [pending], other_pending: 0 }) }, 'e1', true).nativeElement as HTMLElement;
    const ids = el.querySelector('section.ids')?.textContent ?? '';
    expect(ids).toContain('Sandbox');
    expect(ids).toContain('Exec States Task');
    expect(ids).not.toContain('execstates');
    expect(el.querySelector('h1')?.textContent?.trim()).toBe('Exec States Task');
    expect(TestBed.inject(Title).getTitle()).toBe('Exec States Task · Job Board · Gradient');
  });

  it('names the task of an exec job and the title by the task display name', () => {
    const exec: BoardExecJob = { id: 'x1', project: 'o1', entry_point: 'apps.run', task: 'exectask', task_display_name: 'Exec Task', node_index: 0, nodes: 1 };
    const job: AssignedJobDetail = {
      ...DETAIL,
      kind: 2,
      exec_job: exec,
      evaluation: { repository: 'r', commit: 'c', wildcard: 'w', status: 'Completed', task: 'exectask', task_display_name: 'Exec Task' },
    };
    const el = setup({ getJob: () => of(job) }).nativeElement as HTMLElement;
    expect(el.querySelector('h1')?.textContent?.trim()).toBe('Exec Task: apps.run');
    expect(el.querySelector('section.exec a.task-link')?.textContent?.trim()).toBe('Exec Task');
  });
});

describe('BoardJobDetailComponent - project access and evaluation', () => {
  it('shows the project and task as text and hides the log link without project access', () => {
    const el = setup({ getJob: () => of({ ...EVAL_DETAIL, can_view_project: false }) }).nativeElement as HTMLElement;
    expect(el.querySelector('section.ids .project-link')).toBeNull();
    expect(el.querySelector('section.ids .project-name')?.textContent).toBe('Project One');
    expect(el.querySelector('section.ids .log-link')).toBeNull();
    expect(el.querySelector('section.evaluation .task-link')).toBeNull();
    expect(el.querySelector('section.evaluation')?.textContent).toContain('ci');
  });

  it('shows the evaluation status and links the retry job', () => {
    const job: AssignedJobDetail = { ...EVAL_DETAIL, retried_by: 'job-2', evaluation: { ...EVAL_DETAIL.evaluation!, status: 'Failed' } };
    const el = setup({ getJob: () => of(job) }).nativeElement as HTMLElement;
    expect(el.querySelector('section.evaluation .evaluation-status')?.textContent?.trim()).toBe('Failed');
    expect(el.querySelector('section.ids .retry-link')?.getAttribute('href')).toBe('/board/jobs/job-2');
  });
});
