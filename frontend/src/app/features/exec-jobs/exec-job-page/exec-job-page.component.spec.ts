/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
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
    retries_left: 0,
    created_at: '2026-01-01T00:00:00',
    worker: 'w1',
    attempts: [{ number: 1, worker: 'w1', started_at: '2026-01-01T00:00:01' }],
    phases: [{ name: 'epoch-3', attempt: 1, created_at: '2026-01-01T00:00:02' }],
    ssh_command: sshCommand,
    forward_commands: sshCommand ? [FORWARD] : [],
  };
}

function setup(
  status: ExecJobStatus,
  sshCommand: string | null = SSH,
): { fixture: ComponentFixture<ExecJobPageComponent>; abort: ReturnType<typeof vi.fn> } {
  const abort = vi.fn(() => of('Success'));
  const route = {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }) },
    parent: { data: of({ taskAccess: { access: { managed: false, canEdit: true, canTrigger: true } } }) },
  } as unknown as ActivatedRoute;
  TestBed.configureTestingModule({
    imports: [ExecJobPageComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: route },
      {
        provide: ExecJobsService,
        useValue: { get: () => of(job(status, sshCommand)), log: () => of('hello'), abort },
      },
    ],
  });
  const fixture = TestBed.createComponent(ExecJobPageComponent);
  fixture.detectChanges();
  return { fixture, abort };
}

function abortButton(root: HTMLElement): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) => (b.textContent ?? '').includes('Abort'));
}

describe('ExecJobPageComponent', () => {
  it('offers no abort for a finished job', () => {
    const { fixture } = setup('completed');
    expect(abortButton(fixture.nativeElement)).toBeUndefined();
  });

  it('puts abort among the page actions, which phones show icon-only', () => {
    const { fixture } = setup('running');
    const actions = (fixture.nativeElement as HTMLElement).querySelector('.page-layout__actions.phone-icon-buttons');
    const button = actions ? abortButton(actions as HTMLElement) : undefined;
    expect(button?.querySelector('.gr-button__icon')).toBeTruthy();
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
          { provide: ExecJobsService, useValue: { get, log: () => of('hello'), abort: vi.fn() } },
        ],
      });
      const fixture = TestBed.createComponent(ExecJobPageComponent);
      fixture.detectChanges();

      vi.advanceTimersByTime(2000);
      fixture.detectChanges();

      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('epoch-3');
      expect(text).not.toContain('Bad Gateway');
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows the phases and the log', () => {
    const { fixture } = setup('completed');
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('epoch-3');
    expect(text).toContain('hello');
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

    function openTerminalTab(fixture: ComponentFixture<ExecJobPageComponent>): HTMLElement {
      const root = fixture.nativeElement as HTMLElement;
      buttonNamed(root, 'Terminal')?.click();
      fixture.detectChanges();
      return root;
    }

    const path = (url: string): string => url.slice(url.indexOf('/exec-jobs/'));

    it('offers the console, attach, a new shell and the ssh commands for a running job', () => {
      const { fixture } = setup('running');
      const root = openTerminalTab(fixture);
      expect(buttonNamed(root, 'Console')).toBeTruthy();
      expect(buttonNamed(root, 'Attach to program')).toBeTruthy();
      expect(buttonNamed(root, 'New shell')).toBeTruthy();
      const copied = Array.from(root.querySelectorAll('gr-copy-field input')).map((i) => (i as HTMLInputElement).value);
      expect(copied).toEqual([SSH, FORWARD]);
      expect(opened.map(path)).toEqual(['/exec-jobs/j1/console']);
    });

    it('attaches to the program or opens a new shell', () => {
      const { fixture } = setup('running');
      const root = openTerminalTab(fixture);
      buttonNamed(root, 'Attach to program')?.click();
      fixture.detectChanges();
      buttonNamed(root, 'New shell')?.click();
      fixture.detectChanges();
      expect(opened.map(path)).toEqual([
        '/exec-jobs/j1/console',
        '/exec-jobs/j1/shell?attach=true',
        '/exec-jobs/j1/shell',
      ]);
    });

    it('offers no terminal for a completed job', () => {
      const { fixture } = setup('completed');
      const root = fixture.nativeElement as HTMLElement;
      expect(buttonNamed(root, 'Terminal')).toBeUndefined();
      expect(buttonNamed(root, 'Console')).toBeUndefined();
    });

    it('offers only the console without ssh', () => {
      const { fixture } = setup('running', null);
      const root = openTerminalTab(fixture);
      expect(buttonNamed(root, 'Console')).toBeTruthy();
      expect(buttonNamed(root, 'Attach to program')).toBeUndefined();
      expect(buttonNamed(root, 'New shell')).toBeUndefined();
      expect(root.querySelector('gr-copy-field')).toBeNull();
    });
  });
});
