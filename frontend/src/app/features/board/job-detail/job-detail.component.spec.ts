/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { EMPTY, of, throwError } from 'rxjs';
import { BoardJobDetailComponent } from './job-detail.component';
import { BoardService, DispatchedJobDetail, PendingJobSummary } from '@core/services/board.service';
import { EvaluationsService } from '@core/services/evaluations.service';

const DETAIL: DispatchedJobDetail = {
  id: 'job-1',
  kind: 1,
  organization: 'o1',
  organization_name: 'org-one',
  worker_id: 'w1',
  score: 12.5,
  dispatched_at: '2026-06-08T00:01:00Z',
  build_id: null,
  evaluation_id: 'e1',
  pname: null,
  queued_at: '2026-06-08T00:00:00Z',
  finished_at: null,
  score_breakdown: { rules: { wait: 3.5, missing: -1.2 }, total: 12.5 },
  worker_context: {
    architectures: ['x86_64-linux'],
    system_features: ['kvm'],
    capabilities: { core: true, federate: false, fetch: true, eval: false, build: true, cache: true },
    cpu_count: 8,
    cpu_core_score: 1.5,
    ram_total_mb: 32000,
    ram_free_mb: 16000,
    cpu_usage_pct: 42,
    disk_speed_mbps: null,
    network_speed_mbps: 940,
  },
  job_context: {
    kind: 'Build',
    architecture: 'x86_64-linux',
    missing_count: 5,
    missing_nar_size: 1024,
    org_work_share: 0.25,
    rescore_count: 2,
    queued_at: '2026-06-08T00:00:00Z',
    ready_at: '2026-06-08T00:00:30Z',
    dependency_count: 3,
    pname: 'foo',
    closure_size: 2048,
    prefer_local_build: true,
    is_fixed_output: false,
    history: {
      peak_ram_mb: 777,
      avg_cpu_time_ms: 1200,
      build_time_ms: 4200,
      avg_disk_bytes: 5000,
      oom_rate: 0,
      samples: 4,
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
    network_mbps: { w5m: 1, w1h: 2, w24h: 3 },
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
  previous_attempts: [],
};

const EVAL_DETAIL: DispatchedJobDetail = {
  ...DETAIL,
  kind: 0,
  job_context: {
    kind: 'Eval',
    architecture: '',
    missing_count: null,
    missing_nar_size: null,
    org_work_share: null,
    rescore_count: 0,
    queued_at: '2026-06-08T00:00:00Z',
    ready_at: '2026-06-08T00:00:30Z',
    fetch_flake: true,
  },
};

const PENDING: PendingJobSummary = {
  kind: 1,
  organization: 'o1',
  evaluation_id: 'e1',
  build_id: 'b9',
  queued_at: '2026-06-08T00:00:00Z',
  dependency_count: 3,
  pname: null,
};

function setup(board: Partial<BoardService> = {}, id = 'job-1'): ComponentFixture<BoardJobDetailComponent> {
  TestBed.configureTestingModule({
    imports: [BoardJobDetailComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: { get: () => id } } } },
      {
        provide: BoardService,
        useValue: {
          getJob: () => of(DETAIL),
          getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
          ...board,
        },
      },
      { provide: EvaluationsService, useValue: { getBuild: () => EMPTY } },
    ],
  });
  const fixture = TestBed.createComponent(BoardJobDetailComponent);
  fixture.detectChanges();
  return fixture;
}

function noJsonDump(el: HTMLElement): void {
  expect(el.querySelectorAll('pre').length).toBe(0);
}

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
    expect(caps.textContent).toContain('fetch');
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
    const drv = el.querySelector('.ctx.job .drv-row') as HTMLElement;
    expect(drv).toBeTruthy();
    expect(drv.textContent).toContain('foo');
    expect(drv.textContent).toContain('/nix/store/xxx-foo');
  });

  it('links the worker id to the org-scoped worker metrics page', () => {
    const el = setup().nativeElement as HTMLElement;
    const link = el.querySelector('.ids .worker-link') as HTMLAnchorElement;
    expect(link).toBeTruthy();
    expect(link.getAttribute('href')).toBe('/organization/org-one/workers/w1/metrics');
  });

  it('shows the architecture for build jobs', () => {
    const el = setup().nativeElement as HTMLElement;
    const job = el.querySelector('section.ctx.job') as HTMLElement;
    expect(job.textContent).toContain('Architecture');
  });

  it('hides the architecture for eval jobs', () => {
    const el = setup({ getJob: () => of(EVAL_DETAIL) }).nativeElement as HTMLElement;
    const job = el.querySelector('section.ctx.job') as HTMLElement;
    expect(job.textContent).toContain('Eval');
    expect(job.textContent).not.toContain('Architecture');
  });
});

describe('BoardJobDetailComponent - pending fallback', () => {
  it('renders a limited pending view when the job is not yet dispatched', () => {
    const el = setup(
      {
        getJob: () => throwError(() => new Error('not found')),
        getPendingJobs: () => of({ jobs: [PENDING], other_pending: 0 }),
      },
      'e1',
    ).nativeElement as HTMLElement;
    expect(el.textContent).toContain('Pending job');
    expect(el.textContent).not.toContain('Score breakdown');
  });

  it('shows "Job not found" when neither dispatched nor pending', () => {
    const el = setup(
      {
        getJob: () => throwError(() => new Error('not found')),
        getPendingJobs: () => of({ jobs: [], other_pending: 0 }),
      },
      'missing',
    ).nativeElement as HTMLElement;
    expect(el.textContent).toContain('Job not found');
  });
});

describe('BoardJobDetailComponent - previous build attempts', () => {
  const WITH_ATTEMPTS: DispatchedJobDetail = {
    ...DETAIL,
    previous_attempts: [
      { dispatched_job_id: 'dj-a1', substitute: false, outcome: 3, reason: 5, created_at: '2026-06-08T00:00:00Z' },
      { dispatched_job_id: 'dj-a2', substitute: true,  outcome: 2, reason: null, created_at: '2026-06-08T00:01:00Z' },
    ],
  };

  it('renders the Previous Build Attempts section when there are multiple attempts', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    expect(el.textContent).toContain('Previous Build Attempts');
  });

  it('renders one row per attempt', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.attempts') as HTMLElement;
    const rows = section.querySelectorAll('tbody tr');
    expect(rows.length).toBe(2);
  });

  it('shows mode and outcome labels for each attempt', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.attempts') as HTMLElement;
    expect(section.textContent).toContain('build');
    expect(section.textContent).toContain('failed');
    expect(section.textContent).toContain('substitute');
    expect(section.textContent).toContain('substituted');
  });

  it('each row links to the dispatched job', () => {
    const el = setup({ getJob: () => of(WITH_ATTEMPTS) }).nativeElement as HTMLElement;
    const section = el.querySelector('section.attempts') as HTMLElement;
    const rows = section.querySelectorAll('tbody tr');
    expect(rows[0].getAttribute('ng-reflect-router-link')).toContain('dj-a1');
    expect(rows[1].getAttribute('ng-reflect-router-link')).toContain('dj-a2');
  });

  it('hides the section when there is only one attempt', () => {
    const singleAttempt: DispatchedJobDetail = {
      ...DETAIL,
      previous_attempts: [
        { dispatched_job_id: 'dj-a1', substitute: false, outcome: 1, reason: null, created_at: '2026-06-08T00:00:00Z' },
      ],
    };
    const el = setup({ getJob: () => of(singleAttempt) }).nativeElement as HTMLElement;
    expect(el.textContent).not.toContain('Previous Build Attempts');
  });
});
