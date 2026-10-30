/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';
import type { ExecJobResponse, ExecJobStatus } from '@core/models';
import { ExecJobsService } from '@core/services/exec-jobs.service';
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
    retries_left: 0,
    autostart: true,
    keep_vm_running: false,
    created_at: '2026-01-01T00:00:00',
    worker: 'w1',
    attempts: [{ number: 1, worker: 'w1', started_at: '2026-01-01T00:00:01' }],
    phases: [{ name: 'epoch-3', attempt: 1, created_at: '2026-01-01T00:00:02' }],
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
  run: ReturnType<typeof vi.fn>;
  log: ReturnType<typeof vi.fn>;
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
  const run = vi.fn(() => of('Success'));
  const log = vi.fn(() => of(LOG));
  const keepVmRunning = vi.fn(() => of('Success'));
  const letVmStop = vi.fn(() => of('Success'));
  const route = {
    snapshot: {
      paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }),
      queryParamMap: convertToParamMap(query),
    },
    parent: { data: of({ taskAccess: { access: { managed: false, canEdit: canTrigger, canTrigger } } }) },
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
          log,
          pullRequests: () => of([PULL_REQUEST]),
          outputs: () => of([]),
          abort,
          run,
          keepVmRunning,
          letVmStop,
        },
      },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const fixture = TestBed.createComponent(ExecJobPageComponent);
  fixture.detectChanges();
  return { fixture, abort, run, log, keepVmRunning, letVmStop, navigate };
}

function keepButton(root: HTMLElement): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) => (b.textContent ?? '').includes('Keep VM running'));
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

  describe('keep VM running', () => {
    it('switches on from the info bar', () => {
      const { fixture, keepVmRunning } = setup('running');
      const button = keepButton((fixture.nativeElement as HTMLElement).querySelector('.info-bar-right') as HTMLElement);
      expect(button?.getAttribute('aria-pressed')).toBe('false');
      button?.click();
      expect(keepVmRunning).toHaveBeenCalledWith('j1');
    });

    it('switches off again while the program runs, without a notice', () => {
      const { fixture, letVmStop } = setup('running', SSH, {}, keptJob(null));
      const root = fixture.nativeElement as HTMLElement;
      expect(root.querySelector('.kept-vm')).toBeNull();
      const button = keepButton(root);
      expect(button?.getAttribute('aria-pressed')).toBe('true');
      button?.click();
      expect(letVmStop).toHaveBeenCalledWith('j1');
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

    it('shows a refused switch', () => {
      const { fixture, keepVmRunning } = setup('running');
      keepVmRunning.mockReturnValue(throwError(() => new Error('the exec job has already finished')));
      keepButton(fixture.nativeElement)?.click();
      fixture.detectChanges();
      expect((fixture.nativeElement as HTMLElement).querySelector('gr-message-banner')?.textContent)
        .toContain('the exec job has already finished');
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
    expect(root.querySelector('.log-area')?.textContent).toContain('waits for a manual start');
    expect(abortButton(root)).toBeTruthy();
    Array.from(root.querySelectorAll<HTMLButtonElement>('.info-bar-right button'))
      .find((b) => (b.textContent ?? '').includes('Run'))
      ?.click();
    expect(run).toHaveBeenCalledWith('j1');
  });

  it('puts abort at the end of the info bar', () => {
    const { fixture } = setup('running');
    const actions = (fixture.nativeElement as HTMLElement).querySelector('.info-bar-right');
    expect(actions ? abortButton(actions as HTMLElement) : undefined).toBeTruthy();
  });

  it('runs a finished job again and shows a refusal', () => {
    const { fixture, run } = setup('failed');
    run.mockReturnValue(throwError(() => new Error('exec jobs are disabled for this task')));
    const root = fixture.nativeElement as HTMLElement;
    Array.from(root.querySelectorAll('.info-bar-right button'))
      .find((b) => (b.textContent ?? '').includes('Run again'))
      ?.dispatchEvent(new MouseEvent('click'));
    fixture.detectChanges();
    expect(run).toHaveBeenCalledWith('j1');
    expect(root.querySelector('gr-message-banner')?.textContent).toContain('exec jobs are disabled for this task');
  });

  it('names the worker and keeps its id on hover', () => {
    const { fixture } = setup('running', SSH, {}, { ...job('running'), worker_name: 'gpu-box' });
    const worker = (fixture.nativeElement as HTMLElement).querySelector('.info-bar .worker');
    expect(worker?.textContent?.trim()).toBe('gpu-box');
    expect(worker?.getAttribute('title')).toBe('w1');
  });

  it('shows the guest size of the job', () => {
    const { fixture } = setup('completed');
    const guest = (fixture.nativeElement as HTMLElement).querySelector('.info-bar')?.textContent ?? '';
    expect(guest).toContain('2 CPUs · 500 MB memory · 2048 MB disk');
  });

  it('opens the job menu of the task page from the log entry, by right click or its dots', () => {
    const { fixture } = setup('running');
    const root = fixture.nativeElement as HTMLElement;
    const log = root.querySelector('.log-sidebar .sidebar-item') as HTMLElement;
    log.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 5, clientY: 5 }));
    fixture.detectChanges();
    const items = () => Array.from(document.querySelectorAll<HTMLElement>('.gr-menu__item'))
      .map((item) => (item.textContent ?? '').replace(item.querySelector('.gr-menu__icon')?.textContent ?? '', '').trim());
    expect(items()).toEqual(['Show job', 'Terminal', 'Abort']);
    expect(log.querySelector('.sidebar-kebab')).not.toBeNull();

    Array.from(document.querySelectorAll<HTMLElement>('.gr-menu__item')).find((item) => item.textContent?.includes('Abort'))!.click();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('abort this exec job');
  });

  it('links a finished job to its assignment on the Job Board', () => {
    const { fixture } = setup('completed', SSH, {}, { ...job('completed'), dispatched_job: 'dj1' });
    const item = fixture.componentInstance.jobMenu().find((i) => i.label === 'Show job');
    expect(fixture.componentInstance.jobMenu().map((i) => i.label)).toEqual(['Show job']);
    expect(item?.routerLink).toEqual(['/board', 'jobs', 'dj1']);
  });

  it('asks in the dialog before it aborts', () => {
    const { fixture, abort } = setup('running');
    abortButton(fixture.nativeElement)?.click();
    fixture.detectChanges();
    expect(abort).not.toHaveBeenCalled();

    const confirm = Array.from(document.querySelectorAll('.gr-dialog button'))
      .find((b) => (b.textContent ?? '').includes('Abort')) as HTMLButtonElement | undefined;
    confirm?.click();
    expect(abort).toHaveBeenCalledWith('j1');
  });

  it('keeps updating after one failed poll', () => {
    vi.useFakeTimers();
    try {
      const get = vi.fn()
        .mockReturnValueOnce(throwError(() => new Error('Bad Gateway')))
        .mockReturnValue(of(job('running')));
      const route = {
        snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }) },
        parent: { data: of({ taskAccess: { access: { managed: false, canEdit: true, canTrigger: true } } }) },
      } as unknown as ActivatedRoute;
      TestBed.configureTestingModule({
        imports: [ExecJobPageComponent],
        providers: [
          provideRouter([]),
          { provide: ActivatedRoute, useValue: route },
          { provide: ExecJobsService, useValue: { get, log: () => of('hello'), pullRequests: () => of([]), outputs: () => of([]), abort: vi.fn() } },
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
      const log = vi.fn().mockReturnValueOnce(of('hello\n')).mockReturnValue(of('hello\nbye\n'));
      const route = {
        snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }) },
        parent: { data: of({ taskAccess: { access: { managed: false, canEdit: true, canTrigger: true } } }) },
      } as unknown as ActivatedRoute;
      TestBed.configureTestingModule({
        imports: [ExecJobPageComponent],
        providers: [
          provideRouter([]),
          { provide: ActivatedRoute, useValue: route },
          { provide: ExecJobsService, useValue: { get: () => of(job('running')), log, pullRequests: () => of([]), outputs: () => of([]), abort: vi.fn() } },
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
      const { fixture, log, navigate } = setup('completed', SSH, {}, twoNodeJob('completed'));
      expect(log).toHaveBeenLastCalledWith('j1', undefined, 0);
      nodeItem(fixture.nativeElement, 1)?.click();
      fixture.detectChanges();
      expect(log).toHaveBeenLastCalledWith('j1', undefined, 1);
      expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { node: 1 } }));
      expect(nodeItem(fixture.nativeElement, 1)?.classList).toContain('selected');
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
      sidebarItem(root, 'Terminal')?.click();
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
