/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { EMPTY, type Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import type { ExecJobLog, ExecJobResponse, ExecJobStatus } from '@core/models';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { ApiError } from '@core/services/api.service';
import { MessageService } from '@gradient/ui/ui';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { ExecJobPageComponent } from './exec-job-page.component';

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    options = {};
    loadAddon(): void {}
    open(): void {}
    write(): void {}
    reset(): void {}
    dispose(): void {}
    onData() {
      return { dispose: () => undefined };
    }
    onResize() {
      return { dispose: () => undefined };
    }
  },
}));

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit(): void {}
  },
}));

const SSH = 'ssh -p 2222 acme/j1@ci.example.com';
const FORWARD = 'ssh -p 2222 -N -L 6006:127.0.0.1:6006 acme/j1@ci.example.com';
const LOG = 'hello\n::gradient-phase:: epoch-3\nbye\n';
const PULL_REQUEST = {
  url: 'https://github.com/acme/infra/pull/7',
  branch: 'update/flake',
  number: 7,
  integration: 'github',
  created_at: '2026-01-01T00:00:04',
};

function fullLog(log: string): ExecJobLog {
  return { offset: 0, log, total_bytes: new TextEncoder().encode(log).length };
}

function job(status: ExecJobStatus, sshCommand: string | null = SSH): ExecJobResponse {
  return {
    id: 'j1',
    status,
    entry_point: 'apps.x86_64-linux.train',
    evaluation: 'e1',
    task: 'demo',
    project: 'acme',
    nodes: 1,
    labels: [],
    devices: [],
    guest: { cpus: 2, memory_mb: 500, disk_mb: 2048 },
    architecture: 'x86_64-linux',
    mounts: [],
    secrets: [],
    network: false,
    ssh: true,
    ports: [6006],
    retries: 0,
    retries_left: 0,
    autostart: true,
    keep_vm_running: false,
    created_at: '2026-01-01T00:00:00',
    worker: 'w1',
    attempts: [{ number: 1, worker: 'w1', started_at: '2026-01-01T00:00:01' }],
    phases: [{ name: 'epoch-3', attempt: 1, created_at: '2026-01-01T00:00:02' }],
    timeline: [{ seq: 0, parent_seq: null, phase: 'epoch-3', start_ms: 1000, end_ms: 4000, paths: 0, bytes: 0 }],
    ssh_command: sshCommand,
    forward_commands: sshCommand ? [FORWARD] : [],
    tokens: [{ attempt: 1, created_at: '2026-01-01T00:00:01', expires_at: '2026-01-01T02:00:01', revoked: true }],
    started_evaluations: [{ evaluation: 'e2', task: 'deploy', status: 'Completed', created_at: '2026-01-01T00:00:03' }],
    outputs_size: 0,
    node_jobs: [{ node: 0, id: 'j1', status, worker: 'w1', attempts: [] }],
  };
}

function twoNodeJob(status: ExecJobStatus): ExecJobResponse {
  return {
    ...job(status),
    nodes: 2,
    node_jobs: [
      { node: 0, id: 'j1', status, worker: 'w1', guest_address: '10.233.0.2', master_port: 29500, attempts: [] },
      { node: 1, id: 'j1n1', status, worker: 'w2', guest_address: '10.234.0.2', master_port: 29500, attempts: [] },
    ],
  };
}

function keptJob(exitCode: number | null): ExecJobResponse {
  return { ...job('running'), keep_vm_running: true, program_exit_code: exitCode };
}

interface Setup {
  fixture: ComponentFixture<ExecJobPageComponent>;
  abort: ReturnType<typeof vi.fn>;
  confirm: ReturnType<typeof vi.fn>;
  run: ReturnType<typeof vi.fn>;
  logAfter: ReturnType<typeof vi.fn>;
  keepVmRunning: ReturnType<typeof vi.fn>;
  letVmStop: ReturnType<typeof vi.fn>;
  navigate: ReturnType<typeof vi.spyOn>;
}

function setup(
  status: ExecJobStatus,
  sshCommand: string | null = SSH,
  query: Record<string, string> = {},
  response?: ExecJobResponse,
  canTrigger = true,
): Setup {
  const abort = vi.fn(() => of('Success'));
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  const run = vi.fn(() => of('Success'));
  const logAfter = vi.fn(() => of(fullLog(LOG)));
  const keepVmRunning = vi.fn(() => of('Success'));
  const letVmStop = vi.fn(() => of('Success'));
  const route = {
    snapshot: {
      paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }),
      queryParamMap: convertToParamMap(query),
    },
    parent: { data: of(taskAccessData({ managed: false, canEdit: canTrigger, canTrigger })) },
  } as unknown as ActivatedRoute;
  TestBed.configureTestingModule({
    imports: [ExecJobPageComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: route },
      {
        provide: ExecJobsService,
        useValue: {
          get: () => of(response ?? job(status, sshCommand)),
          logAfter,
          pullRequests: () => of([PULL_REQUEST]),
          outputs: () => of([]),
          abort,
          run,
          keepVmRunning,
          letVmStop,
        },
      },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const fixture = TestBed.createComponent(ExecJobPageComponent);
  fixture.detectChanges();
  return { fixture, abort, confirm, run, logAfter, keepVmRunning, letVmStop, navigate };
}

function toasts() {
  return TestBed.inject(MessageService).messages();
}

function keepButton(root: HTMLElement): HTMLButtonElement | undefined {
  return root.querySelector<HTMLButtonElement>('.info-bar-right button[aria-pressed]') ?? undefined;
}

function nodeItem(root: HTMLElement, node: number): HTMLElement | undefined {
  return Array.from(root.querySelectorAll<HTMLElement>('.log-sidebar .node-item'))
    .find((item) => item.querySelector('.sidebar-item-label')?.textContent?.trim().startsWith(`Node ${node}`));
}

async function settle(fixture: ComponentFixture<ExecJobPageComponent>): Promise<void> {
  for (let pass = 0; pass < 2; pass++) await new Promise((resolve) => setTimeout(resolve));
  fixture.detectChanges();
}

function abortButton(root: HTMLElement): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) => (b.textContent ?? '').includes('Abort'));
}

describe('ExecJobPageComponent', () => {
  it('offers no abort and no keep VM running for a finished job', () => {
    const { fixture } = setup('completed');
    expect(abortButton(fixture.nativeElement)).toBeUndefined();
    expect(keepButton(fixture.nativeElement)).toBeUndefined();
  });

  it.each([
    ['running', 1, '1 retry left'],
    ['running', 0, undefined],
    ['failed', 2, undefined],
  ] as const)('shows the retries left of a %s job with %i left only while it can still retry', (status, left, chip) => {
    const { fixture } = setup(status, SSH, {}, { ...job(status), retries: 2, retries_left: left });
    expect((fixture.nativeElement as HTMLElement).querySelector('.retries-left')?.textContent?.trim()).toBe(chip);
  });

  it('offers abort but no keep VM running before a VM exists', () => {
    for (const status of ['created', 'queued'] as const) {
      const { fixture } = setup(status);
      expect(abortButton(fixture.nativeElement)).toBeDefined();
      expect(keepButton(fixture.nativeElement)).toBeUndefined();
      TestBed.resetTestingModule();
    }
  });

  describe('keep VM running', () => {
    it('switches on from the info bar', () => {
      const { fixture, keepVmRunning } = setup('running');
      const button = keepButton((fixture.nativeElement as HTMLElement).querySelector('.info-bar-right') as HTMLElement);
      expect(button?.getAttribute('aria-pressed')).toBe('false');
      expect(button?.textContent).toContain('Keep VM running');
      button?.click();
      expect(keepVmRunning).toHaveBeenCalledWith('j1');
      expect(toasts()).toContainEqual(expect.objectContaining({ severity: 'success', summary: 'VM running after program exit' }));
    });

    it('switches off again while the program runs, without a notice', () => {
      const { fixture, letVmStop } = setup('running', SSH, {}, keptJob(null));
      const root = fixture.nativeElement as HTMLElement;
      expect(root.querySelector('.kept-vm')).toBeNull();
      const button = keepButton(root);
      expect(button?.getAttribute('aria-pressed')).toBe('true');
      expect(button?.textContent).toContain('VM left running');
      button?.click();
      expect(letVmStop).toHaveBeenCalledWith('j1');
      expect(toasts()).toContainEqual(expect.objectContaining({ severity: 'success', summary: 'VM shutdown at program end' }));
    });

    it('shows the exit code of a kept VM and lets the VM stop', () => {
      const { fixture, letVmStop } = setup('running', SSH, {}, keptJob(0));
      const notice = (fixture.nativeElement as HTMLElement).querySelector('.kept-vm');
      expect(notice?.textContent).toContain('Program exited with code 0. The VM keeps running.');
      Array.from(notice?.querySelectorAll('button') ?? [])
        .find((b) => (b.textContent ?? '').includes('Let VM stop'))
        ?.click();
      expect(letVmStop).toHaveBeenCalledWith('j1');
    });

    it('shows a refused switch as a toast that polling does not clear', () => {
      const { fixture, keepVmRunning } = setup('running');
      keepVmRunning.mockReturnValue(throwError(() => new ApiError('the exec job has already finished', 409)));
      keepButton(fixture.nativeElement)?.click();
      fixture.detectChanges();
      expect(toasts()).toContainEqual(expect.objectContaining({
        severity: 'error',
        summary: 'Failed to keep VM running',
        detail: 'the exec job has already finished',
      }));
      expect(fixture.componentInstance.errorMessage()).toBeNull();
    });
  });

  it('starts a job that waits for its manual start', () => {
    const waiting: ExecJobResponse = {
      ...job('created'),
      autostart: false,
      worker: null,
      attempts: [],
      waiting_reason: { kind: 'manual_start' },
    };
    const { fixture, run } = setup('created', SSH, {}, waiting);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.log-area')?.textContent).toContain('Waiting for manual start');
    expect(root.querySelector('.info-bar gr-status-badge')?.textContent?.trim().toLowerCase()).toBe('manual');
    expect(abortButton(root)).toBeTruthy();
    root.querySelector<HTMLButtonElement>('.log-area gr-waiting-notice button')?.click();
    expect(run).toHaveBeenCalledWith('j1');
    expect(toasts()).toContainEqual(expect.objectContaining({ severity: 'success', summary: 'Exec job added to queue' }));
  });

  it('puts abort at the end of the info bar', () => {
    const { fixture } = setup('running');
    const actions = (fixture.nativeElement as HTMLElement).querySelector('.info-bar-right');
    expect(actions ? abortButton(actions as HTMLElement) : undefined).toBeTruthy();
  });

  it('runs a finished job again and shows a refusal', () => {
    const { fixture, run } = setup('failed');
    run.mockReturnValue(throwError(() => new ApiError('exec jobs are disabled for this task', 403)));
    const root = fixture.nativeElement as HTMLElement;
    Array.from(root.querySelectorAll('.info-bar-right button'))
      .find((b) => (b.textContent ?? '').includes('Run again'))
      ?.dispatchEvent(new MouseEvent('click'));
    fixture.detectChanges();
    expect(run).toHaveBeenCalledWith('j1');
    expect(toasts()).toContainEqual(expect.objectContaining({
      summary: 'Failed to start exec job',
      detail: 'exec jobs are disabled for this task',
    }));
  });

  it('drops the log of the previous run while the job waits in the queue again', () => {
    const { fixture } = setup('queued');
    expect(fixture.componentInstance.logLines()).toEqual([]);
  });

  it('keeps the full entry point of the current crumb on hover', () => {
    const { fixture } = setup('running');
    const current = (fixture.nativeElement as HTMLElement).querySelector('.sidebar-breadcrumb .breadcrumb-current');
    expect(current?.getAttribute('title')).toBe('apps.x86_64-linux.train');
  });

  it('names the entry point in the crumb only, not again in the log header', async () => {
    const { fixture } = setup('completed');
    await settle(fixture);
    const header = (fixture.nativeElement as HTMLElement).querySelector('gr-log-viewer .log-header');
    expect(header?.textContent).not.toContain('apps.x86_64-linux.train');
    expect(header?.textContent).toContain('3 lines');
  });

  it('shows no times of an ended attempt while the job waits to run again', () => {
    const requeued: ExecJobResponse = {
      ...job('queued'),
      attempts: [{ number: 1, worker: 'w1', started_at: '2026-01-01T00:00:01', program_started_at: '2026-01-01T00:00:04', ended_at: '2026-01-01T00:05:00', end_reason: 'aborted' }],
    };
    const { fixture } = setup('queued', SSH, {}, requeued);
    const labels = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.info-bar .metric-label'))
      .map((label) => label.textContent?.trim());
    expect(labels).not.toContain('Program run');
    expect(labels).not.toContain('Started');
  });

  it('names the worker and keeps its id on hover', () => {
    const { fixture } = setup('running', SSH, {}, { ...job('running'), worker_name: 'gpu-box' });
    const worker = (fixture.nativeElement as HTMLElement).querySelector('.info-bar .worker');
    expect(worker?.textContent?.trim()).toBe('gpu-box');
    expect(worker?.getAttribute('title')).toBe('w1');
  });

  it('measures the program run of the latest attempt without VM boot, the span its timeout counts', () => {
    const timedOut: ExecJobResponse = {
      ...job('failed'),
      started_at: '2026-01-01T00:00:20',
      finished_at: '2026-01-01T00:00:24',
      attempts: [
        { number: 1, worker: 'w1', started_at: '2026-01-01T00:00:01', program_started_at: '2026-01-01T00:00:03', ended_at: '2026-01-01T00:00:05', end_reason: 'timeout' },
        { number: 2, worker: 'w1', started_at: '2026-01-01T00:00:10', program_started_at: '2026-01-01T00:00:12', ended_at: '2026-01-01T00:00:15', end_reason: 'timeout' },
      ],
    };
    const { fixture } = setup('failed', SSH, {}, timedOut);
    const metrics = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.info-bar .info-metric'))
      .map((metric) => Array.from(metric.querySelectorAll('.metric-label, .metric-value')).map((part) => part.textContent?.trim()).join(': '));
    expect(metrics).toContain('Program run: 3.0 s');
    expect(metrics.some((metric) => metric.startsWith('First start: '))).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.info-bar .metric-sep')?.textContent?.trim()).toBe('Attempt 2');
    expect(fixture.componentInstance.firstAttemptStartedAt()).toBe('2026-01-01T00:00:01');
  });

  it('names why the latest attempt of a failed job ended in the status badge', () => {
    const lost: ExecJobResponse = {
      ...job('failed'),
      attempts: [
        { number: 1, worker: 'w1', started_at: '2026-01-01T00:00:01', ended_at: '2026-01-01T00:00:05', end_reason: 'timeout' },
        { number: 2, worker: 'w1', started_at: '2026-01-01T00:00:10', ended_at: '2026-01-01T00:00:15', end_reason: 'lost' },
      ],
    };
    const { fixture } = setup('failed', SSH, {}, lost);
    expect((fixture.nativeElement as HTMLElement).querySelector('.info-bar gr-status-badge')?.textContent?.trim()).toBe('Worker lost');
  });

  it('shows the worker timeline with the reported phases from the sidebar', () => {
    const { fixture } = setup('completed');
    const root = fixture.nativeElement as HTMLElement;
    Array.from(root.querySelectorAll<HTMLElement>('.log-sidebar .sidebar-item'))
      .find((item) => item.textContent?.includes('Timeline'))
      ?.querySelector<HTMLElement>('.sidebar-row-link')?.click();
    fixture.detectChanges();
    expect(root.querySelector('gr-log-viewer')).toBeNull();
    expect(root.querySelector('gr-job-timeline .phases')?.textContent).toContain('epoch-3');
  });

  it('keeps the info tab of a waiting job open across polls', async () => {
    vi.useFakeTimers();
    try {
      const queued: ExecJobResponse = {
        ...job('queued'),
        worker: null,
        attempts: [],
        waiting_reason: {
          kind: 'exec_workers',
          unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false, ssh: true },
          connected_workers: 1,
          matching_workers: 0,
        },
      };
      const { fixture } = setup('queued', SSH, {}, queued);
      const root = fixture.nativeElement as HTMLElement;
      Array.from(root.querySelectorAll<HTMLElement>('.log-sidebar .sidebar-item'))
        .find((item) => item.textContent?.includes('Info'))
        ?.querySelector<HTMLElement>('.sidebar-row-link')?.click();
      await vi.advanceTimersByTimeAsync(2000);
      fixture.detectChanges();
      expect(root.querySelector('gr-log-viewer')).toBeNull();
      expect(root.querySelector('app-exec-job-info')).not.toBeNull();
      expect(root.querySelector('.log-area gr-waiting-notice')).not.toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows the guest size once, in the Info tab', () => {
    const { fixture } = setup('completed');
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.info-bar')?.textContent).not.toContain('2 CPUs');
    fixture.componentInstance.showView('info');
    fixture.detectChanges();
    expect(root.querySelector('app-exec-job-info')?.textContent).toContain('2 CPUs · 500 MB memory · 2048 MB disk');
  });

  it('keeps the entry point and the views outside the drawer for phones', () => {
    const { fixture } = setup('running');
    const row = (fixture.nativeElement as HTMLElement).querySelector('.phone-breadcrumb-and-view-tabs-row') as HTMLElement;
    expect(row.querySelector('.breadcrumb-current')?.textContent?.trim()).toBe('apps.x86_64-linux.train');
    const tabs = Array.from(row.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
    expect(tabs.map((tab) => tab.textContent?.trim())).toEqual(['Log', 'Info', 'Timeline', 'Terminal']);
    tabs[1].click();
    fixture.detectChanges();
    expect(fixture.componentInstance.view()).toBe('info');
  });

  it('keeps the chosen view in the URL and opens it again from there', () => {
    const { fixture, navigate } = setup('completed');
    fixture.componentInstance.showView('timeline');
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { view: 'timeline' } }));
    fixture.componentInstance.showView('log');
    expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ queryParams: { view: null } }));

    TestBed.resetTestingModule();
    const reopened = setup('completed', undefined, { view: 'info' });
    expect(reopened.fixture.componentInstance.view()).toBe('info');
  });

  it('starts the breadcrumb at the project list', () => {
    const { fixture } = setup('completed');
    expect(fixture.componentInstance.crumbs()[0].label).toBe('Projects');
  });

  function showJobLink(root: HTMLElement): HTMLAnchorElement | undefined {
    return Array.from(root.querySelectorAll<HTMLAnchorElement>('.info-bar-right a')).find((a) => a.textContent?.includes('Show job'));
  }

  it('links a finished job to its assignment on the Job Board from the header', () => {
    const { fixture } = setup('completed', SSH, {}, { ...job('completed'), dispatched_job: 'dj1' });
    const root = fixture.nativeElement as HTMLElement;
    expect(showJobLink(root)?.getAttribute('href')).toBe('/board/jobs/dj1');
    expect(root.querySelector('.log-sidebar .sidebar-kebab')).toBeNull();
  });

  it('leaves out the job link while the job has no assignment to show', () => {
    const { fixture } = setup('queued');
    expect(showJobLink(fixture.nativeElement)).toBeUndefined();
  });

  it('asks first, naming the entry point, then aborts', () => {
    const { fixture, abort, confirm } = setup('running');
    abortButton(fixture.nativeElement)?.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Abort Exec Job', target: 'apps.x86_64-linux.train' }));
    expect(abort).toHaveBeenCalledWith('j1');
    expect(toasts()).toContainEqual(expect.objectContaining({ severity: 'success', summary: 'Exec job aborted' }));
  });

  it('sends nothing and shows no toast when the abort is cancelled', () => {
    const { fixture, abort, confirm } = setup('running');
    confirm.mockImplementation(() => EMPTY);
    abortButton(fixture.nativeElement)?.click();
    expect(confirm).toHaveBeenCalledOnce();
    expect(abort).not.toHaveBeenCalled();
    expect(toasts()).toEqual([]);
  });

  it('keeps updating after one failed poll', () => {
    vi.useFakeTimers();
    try {
      const get = vi.fn()
        .mockReturnValueOnce(throwError(() => new Error('Bad Gateway')))
        .mockReturnValue(of(job('running')));
      const route = {
        snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }) },
        parent: { data: of(taskAccessData({ managed: false, canEdit: true, canTrigger: true })) },
      } as unknown as ActivatedRoute;
      TestBed.configureTestingModule({
        imports: [ExecJobPageComponent],
        providers: [
          provideRouter([]),
          { provide: ActivatedRoute, useValue: route },
          { provide: ExecJobsService, useValue: { get, logAfter: () => of(fullLog('hello')), pullRequests: () => of([]), outputs: () => of([]), abort: vi.fn() } },
        ],
      });
      const fixture = TestBed.createComponent(ExecJobPageComponent);
      fixture.detectChanges();

      vi.advanceTimersByTime(2000);
      fixture.detectChanges();

      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('apps.x86_64-linux.train');
      expect(text).not.toContain('Bad Gateway');
    } finally {
      vi.useRealTimers();
    }
  });

  it('lists the phases of the log in the sidebar and jumps to one on click', async () => {
    Element.prototype.scrollIntoView = vi.fn();
    const { fixture } = setup('completed');
    await settle(fixture);
    const root = fixture.nativeElement as HTMLElement;
    const phase = root.querySelector<HTMLElement>('.log-sidebar .phase-item');
    expect(phase?.textContent).toContain('epoch-3');

    phase?.click();
    await settle(fixture);
    expect(root.querySelector('gr-log-viewer .log-line--hl')?.getAttribute('data-line')).toBe('2');
  });

  it('renders the log in the shared log viewer, whose search opens on Ctrl+F', async () => {
    const { fixture } = setup('completed');
    await settle(fixture);
    const find = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true });
    document.dispatchEvent(find);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(find.defaultPrevented).toBe(true);
    expect(root.querySelector('gr-log-viewer .log-search-input')).not.toBeNull();
    expect(root.querySelector('gr-log-viewer [data-line="3"] .log-gutter')?.textContent).toBe('3');
  });

  it('keeps an open log search while a poll appends to the log', async () => {
    vi.useFakeTimers();
    try {
      Element.prototype.scrollTo = vi.fn() as unknown as typeof Element.prototype.scrollTo;
      const logAfter = vi.fn().mockReturnValueOnce(of(fullLog('hello\n'))).mockReturnValue(of({ offset: 6, log: 'bye\n', total_bytes: 10 }));
      const route = {
        snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }) },
        parent: { data: of(taskAccessData({ managed: false, canEdit: true, canTrigger: true })) },
      } as unknown as ActivatedRoute;
      TestBed.configureTestingModule({
        imports: [ExecJobPageComponent],
        providers: [
          provideRouter([]),
          { provide: ActivatedRoute, useValue: route },
          { provide: ExecJobsService, useValue: { get: () => of(job('running')), logAfter, pullRequests: () => of([]), outputs: () => of([]), abort: vi.fn() } },
        ],
      });
      const fixture = TestBed.createComponent(ExecJobPageComponent);
      fixture.detectChanges();
      await vi.advanceTimersByTimeAsync(10);
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'f', ctrlKey: true }));

      await vi.advanceTimersByTimeAsync(2000);
      fixture.detectChanges();

      const viewer = (fixture.nativeElement as HTMLElement).querySelector('gr-log-viewer');
      expect(viewer?.querySelector('.log-search-input')).not.toBeNull();
      expect(viewer?.textContent).toContain('bye');
    } finally {
      vi.useRealTimers();
    }
  });

  it('asks a running job only for the log bytes after the ones it holds and replaces the log when the server starts over', async () => {
    vi.useFakeTimers();
    try {
      const { fixture, logAfter } = setup('running');
      logAfter.mockReturnValueOnce(of({ offset: LOG.length, log: 'more\n', total_bytes: LOG.length + 5 }));
      await vi.advanceTimersByTimeAsync(2000);
      expect(logAfter).toHaveBeenLastCalledWith('j1', LOG.length, 1, 0);
      expect(fixture.componentInstance.logLines().at(-1)).toBe('more');

      logAfter.mockReturnValueOnce(of(fullLog('fresh\n')));
      await vi.advanceTimersByTimeAsync(2000);
      expect(logAfter).toHaveBeenLastCalledWith('j1', LOG.length + 5, 1, 0);
      expect(fixture.componentInstance.logLines()).toEqual(['fresh']);
    } finally {
      vi.useRealTimers();
    }
  });

  it('links each opened pull request', () => {
    const { fixture } = setup('completed');
    const link = (fixture.nativeElement as HTMLElement).querySelector<HTMLAnchorElement>(`a[href="${PULL_REQUEST.url}"]`);
    expect(link?.textContent).toContain('#7');
  });

  it('links each started evaluation to its log page', () => {
    const { fixture } = setup('completed');
    const link = (fixture.nativeElement as HTMLElement).querySelector<HTMLAnchorElement>('a[href="/project/acme/log/e2"]');
    expect(link?.querySelector('.sidebar-item-label')?.textContent?.trim()).toBe('deploy');
    expect(link?.querySelector('gr-status-icon')).toBeTruthy();
  });

  it('shows each phase marker in the log as a divider with the phase name', async () => {
    const { fixture } = setup('completed');
    await settle(fixture);
    const log = (fixture.nativeElement as HTMLElement).querySelector('gr-log-viewer');
    expect(log?.querySelector('[role="separator"]')?.textContent?.trim()).toBe('epoch-3');
    expect(log?.textContent).not.toContain('::gradient-phase::');
    expect(log?.textContent).toContain('bye');
  });

  describe('nodes', () => {
    it('lists no nodes for a single node job', () => {
      const { fixture } = setup('running');
      expect((fixture.nativeElement as HTMLElement).querySelector('.node-item')).toBeNull();
    });

    it('lists each node with its guest address', () => {
      const { fixture } = setup('running', SSH, {}, twoNodeJob('running'));
      const items = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.log-sidebar .node-item'));
      expect(items.map((item) => item.querySelector('.item-meta')?.textContent?.trim())).toEqual(['10.233.0.2', '10.234.0.2']);
      expect(items[1].querySelector('gr-status-icon')).toBeTruthy();
      expect(items[1].getAttribute('title')).toBe('w2');
    });

    it('loads the log of a picked node and sets ?node=1', () => {
      const { fixture, logAfter, navigate } = setup('completed', SSH, {}, twoNodeJob('completed'));
      expect(logAfter).toHaveBeenLastCalledWith('j1', undefined, 1, 0);
      nodeItem(fixture.nativeElement, 1)?.click();
      fixture.detectChanges();
      expect(logAfter).toHaveBeenLastCalledWith('j1', undefined, undefined, 1);
      expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { node: 1 } }));
      expect(nodeItem(fixture.nativeElement, 1)?.classList).toContain('selected');
    });

    it('shows the job on node 0 and clears ?node= for a node the job does not have', () => {
      const { fixture, logAfter, navigate } = setup('completed', SSH, { node: '99' }, twoNodeJob('completed'));
      expect(logAfter).toHaveBeenCalledTimes(1);
      expect(logAfter).toHaveBeenCalledWith('j1', undefined, 1, 0);
      expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { node: null } }));
      expect((fixture.nativeElement as HTMLElement).textContent).toContain('apps.x86_64-linux.train');
      expect(nodeItem(fixture.nativeElement, 0)?.classList).toContain('selected');
    });
  });

  describe('terminal', () => {
    const realWs = globalThis.WebSocket;
    let opened: string[];

    beforeEach(() => {
      opened = [];
      (globalThis as unknown as { WebSocket: unknown }).WebSocket = class {
        static readonly CONNECTING = 0;
        static readonly OPEN = 1;
        readyState = 0;
        constructor(url: string) {
          opened.push(url);
        }
        send(): void {}
        close(): void {}
      };
    });

    afterEach(() => {
      (globalThis as unknown as { WebSocket: unknown }).WebSocket = realWs;
    });

    function buttonNamed(root: HTMLElement, name: string): HTMLButtonElement | undefined {
      return Array.from(root.querySelectorAll('button')).find((b) => (b.textContent ?? '').trim() === name);
    }

    function sidebarItem(root: HTMLElement, name: string): HTMLElement | undefined {
      return Array.from(root.querySelectorAll<HTMLElement>('.log-sidebar .sidebar-item'))
        .find((item) => item.querySelector('.sidebar-item-label')?.textContent?.trim() === name);
    }

    function openTerminalTab(fixture: ComponentFixture<ExecJobPageComponent>): HTMLElement {
      const root = fixture.nativeElement as HTMLElement;
      sidebarItem(root, 'Terminal')?.querySelector<HTMLElement>('.sidebar-row-link')?.click();
      fixture.detectChanges();
      return root;
    }

    const path = (url: string): string => url.slice(url.indexOf('/exec/'));

    it('offers the console, attach, a new shell and the ssh commands for a running job', () => {
      const { fixture } = setup('running');
      const root = openTerminalTab(fixture);
      expect(buttonNamed(root, 'Console')).toBeTruthy();
      expect(buttonNamed(root, 'Attach to program')).toBeTruthy();
      expect(buttonNamed(root, 'New shell')).toBeTruthy();
      const copied = Array.from(root.querySelectorAll('gr-copy-field input')).map((i) => (i as HTMLInputElement).value);
      expect(copied).toEqual([SSH, FORWARD]);
      expect(opened.map(path)).toEqual(['/exec/j1/console?node=0']);
    });

    it('hides the ssh commands from a viewer who may not log in', () => {
      const { fixture } = setup('running', SSH, {}, undefined, false);
      const root = openTerminalTab(fixture);
      expect(root.querySelector('gr-copy-field')).toBeNull();
    });

    it('opens straight into the terminal from a link with view=terminal', () => {
      const { fixture } = setup('running', SSH, { view: 'terminal' });
      fixture.detectChanges();
      expect((fixture.nativeElement as HTMLElement).querySelector('gr-terminal')).not.toBeNull();
      expect(opened.map(path)).toEqual(['/exec/j1/console?node=0']);
    });

    it('attaches to the program or opens a new shell', () => {
      const { fixture } = setup('running');
      const root = openTerminalTab(fixture);
      buttonNamed(root, 'Attach to program')?.click();
      fixture.detectChanges();
      buttonNamed(root, 'New shell')?.click();
      fixture.detectChanges();
      expect(opened.map(path)).toEqual([
        '/exec/j1/console?node=0',
        '/exec/j1/shell?node=0&attach=true',
        '/exec/j1/shell?node=0',
      ]);
    });

    it('follows the picked node', () => {
      const { fixture } = setup('running', SSH, {}, twoNodeJob('running'));
      nodeItem(fixture.nativeElement, 1)?.click();
      fixture.detectChanges();
      const root = openTerminalTab(fixture);
      buttonNamed(root, 'Attach to program')?.click();
      fixture.detectChanges();
      expect(opened.map(path)).toEqual(['/exec/j1/console?node=1', '/exec/j1/shell?node=1&attach=true']);
    });

    it('offers no terminal for a completed job', () => {
      const { fixture } = setup('completed');
      const root = fixture.nativeElement as HTMLElement;
      expect(sidebarItem(root, 'Terminal')).toBeUndefined();
      expect(buttonNamed(root, 'Console')).toBeUndefined();
    });

    it('opens the console without a mode switch when there is no ssh', () => {
      const { fixture } = setup('running', null);
      const root = openTerminalTab(fixture);
      expect(opened.map(path)).toEqual(['/exec/j1/console?node=0']);
      expect(root.querySelector('gr-select-button')).toBeNull();
      expect(buttonNamed(root, 'New shell')).toBeUndefined();
      expect(root.querySelector('gr-copy-field')).toBeNull();
    });
  });
});
