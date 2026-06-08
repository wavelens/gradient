/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute } from '@angular/router';
import { EMPTY, of } from 'rxjs';
import { BoardJobDetailComponent } from './job-detail.component';
import { BoardService, DispatchedJobDetail } from '@core/services/board.service';
import { EvaluationsService } from '@core/services/evaluations.service';

const DETAIL: DispatchedJobDetail = {
  id: 'job-1',
  kind: 1,
  organization: 'o1',
  worker_id: 'w1',
  score: 12.5,
  dispatched_at: '2026-06-08T00:01:00Z',
  build_id: null,
  evaluation_id: 'e1',
  queued_at: '2026-06-08T00:00:00Z',
  finished_at: null,
  score_breakdown: { rules: { wait: 3.5, missing: -1.2 }, total: 12.5 },
  worker_context: {
    architectures: ['x86_64-linux'],
    system_features: ['kvm'],
    fetch: true,
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
};

function setup(): ComponentFixture<BoardJobDetailComponent> {
  TestBed.configureTestingModule({
    imports: [BoardJobDetailComponent],
    providers: [
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: { get: () => 'job-1' } } } },
      { provide: BoardService, useValue: { getJob: () => of(DETAIL) } },
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

  it('renders a derivations row with the pname / drv_path', () => {
    const el = setup().nativeElement as HTMLElement;
    const drv = el.querySelector('.ctx.job .drv-row') as HTMLElement;
    expect(drv).toBeTruthy();
    expect(drv.textContent).toContain('foo');
    expect(drv.textContent).toContain('/nix/store/xxx-foo');
  });

  it('shows the job-context kind and history peak_ram_mb', () => {
    const el = setup().nativeElement as HTMLElement;
    const job = el.querySelector('section.ctx.job') as HTMLElement;
    expect(job).toBeTruthy();
    expect(job.textContent).toContain('Build');
    expect(job.textContent).toContain('777');
  });

  it('renders the instance-context windowed table with scalar counts', () => {
    const el = setup().nativeElement as HTMLElement;
    const inst = el.querySelector('section.ctx.instance') as HTMLElement;
    expect(inst).toBeTruthy();
    expect(inst.textContent).toContain('Instance context');
    expect(inst.textContent).toContain('7');
  });
});
