/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import type { ExecJobSummary } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { ExecJobListComponent } from './exec-job-list.component';

function queuedJob(): ExecJobSummary {
  return {
    id: 'j1',
    entry_point: 'apps.x86_64-linux.train',
    status: 'queued',
    waiting_reason: {
      kind: 'exec_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 2,
      matching_workers: 1,
    },
    nodes: 1,
    labels: ['lab-a'],
    devices: [{ name: 'gpu:h100', count: 8 }],
    autostart: true,
    keep_vm_running: false,
  };
}

describe('ExecJobListComponent', () => {
  it('shows a queued job with its status icon, wait reason, labels and devices', () => {
    TestBed.configureTestingModule({
      imports: [ExecJobListComponent],
      providers: [
        provideRouter([]),
        { provide: ExecJobsService, useValue: { forEvaluation: () => of([queuedJob()]) } },
      ],
    });
    const fixture = TestBed.createComponent(ExecJobListComponent);
    fixture.componentRef.setInput('evaluation', 'e1');
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'demo');
    fixture.componentRef.setInput('canTrigger', true);
    fixture.detectChanges();

    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelector('gr-status-icon')?.getAttribute('data-phase')).toBe('queued');
    expect(host.textContent).toContain('Waiting for free devices on 1 worker');
    expect(host.textContent).toContain('lab-a');
    expect(host.textContent).toContain('gpu:h100:8');
    expect(host.querySelector('a.entry-link')?.getAttribute('href')).toBe('/project/acme/task/demo/exec/j1');
    expect(host.querySelector('.entry-kebab')?.getAttribute('aria-label')).toBe('Actions for apps.x86_64-linux.train');
  });

  it('reports every open job that waits with a reason', () => {
    const done: ExecJobSummary = { ...queuedJob(), id: 'j0', status: 'completed' };
    const second: ExecJobSummary = { ...queuedJob(), id: 'j2', status: 'created', waiting_reason: { kind: 'manual_start' } };
    TestBed.configureTestingModule({
      imports: [ExecJobListComponent],
      providers: [
        provideRouter([]),
        { provide: ExecJobsService, useValue: { forEvaluation: () => of([done, queuedJob(), second]) } },
      ],
    });
    const fixture = TestBed.createComponent(ExecJobListComponent);
    const reported: string[][] = [];
    fixture.componentInstance.openExecJobsWaitingWithReason.subscribe((jobs) => reported.push(jobs.map((job) => job.id)));
    fixture.componentRef.setInput('evaluation', 'e1');
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'demo');
    fixture.detectChanges();

    expect(reported).toEqual([['j1', 'j2']]);
    fixture.destroy();
  });

  it('shows a job that waits for its manual start by its wait reason', () => {
    const waiting: ExecJobSummary = {
      ...queuedJob(),
      status: 'created',
      autostart: false,
      waiting_reason: { kind: 'manual_start' },
    };
    TestBed.configureTestingModule({
      imports: [ExecJobListComponent],
      providers: [
        provideRouter([]),
        { provide: ExecJobsService, useValue: { forEvaluation: () => of([waiting]) } },
      ],
    });
    const fixture = TestBed.createComponent(ExecJobListComponent);
    fixture.componentRef.setInput('evaluation', 'e1');
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'demo');
    fixture.detectChanges();

    expect((fixture.nativeElement as HTMLElement).querySelector('.job-detail')?.textContent)
      .toBe('Waiting for manual start');
  });

  it('append last failed attempt to row text for a job queued again', () => {
    const queuedAgain: ExecJobSummary = { ...queuedJob(), waiting_reason: null, end_reason: 'lost' };
    const waitsForDevicesAfterTimeout: ExecJobSummary = { ...queuedJob(), id: 'j2', end_reason: 'timeout' };
    const queuedAfterAbort: ExecJobSummary = { ...queuedJob(), id: 'j3', waiting_reason: null, end_reason: 'aborted' };
    TestBed.configureTestingModule({
      imports: [ExecJobListComponent],
      providers: [
        provideRouter([]),
        { provide: ExecJobsService, useValue: { forEvaluation: () => of([queuedAgain, waitsForDevicesAfterTimeout, queuedAfterAbort]) } },
      ],
    });
    const fixture = TestBed.createComponent(ExecJobListComponent);
    fixture.componentRef.setInput('evaluation', 'e1');
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'demo');
    fixture.detectChanges();

    expect(Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.job-detail'), (detail) => detail.textContent)).toEqual([
      'Queued, last attempt worker lost',
      'Waiting for free devices on 1 worker, last attempt timed out',
      'Queued',
    ]);
  });

  it('keeps polling a running evaluation until its exec jobs appear', () => {
    vi.useFakeTimers();
    try {
      const forEvaluation = vi.fn()
        .mockReturnValueOnce(of([]))
        .mockReturnValue(of([queuedJob()]));
      TestBed.configureTestingModule({
        imports: [ExecJobListComponent],
        providers: [provideRouter([]), { provide: ExecJobsService, useValue: { forEvaluation } }],
      });
      const fixture = TestBed.createComponent(ExecJobListComponent);
      fixture.componentRef.setInput('evaluation', 'e1');
      fixture.componentRef.setInput('evaluationStatus', 'EvaluatingDerivation');
      fixture.componentRef.setInput('project', 'acme');
      fixture.componentRef.setInput('task', 'demo');
      fixture.detectChanges();

      vi.advanceTimersByTime(4000);
      fixture.detectChanges();

      expect((fixture.nativeElement as HTMLElement).textContent).toContain('apps.x86_64-linux.train');
    } finally {
      vi.useRealTimers();
    }
  });

  it('stops polling a building evaluation that has no exec jobs', () => {
    vi.useFakeTimers();
    try {
      const forEvaluation = vi.fn(() => of([]));
      TestBed.configureTestingModule({
        imports: [ExecJobListComponent],
        providers: [provideRouter([]), { provide: ExecJobsService, useValue: { forEvaluation } }],
      });
      const fixture = TestBed.createComponent(ExecJobListComponent);
      fixture.componentRef.setInput('evaluation', 'e1');
      fixture.componentRef.setInput('evaluationStatus', 'Building');
      fixture.componentRef.setInput('project', 'acme');
      fixture.componentRef.setInput('task', 'demo');
      fixture.detectChanges();

      vi.advanceTimersByTime(12000);

      expect(forEvaluation).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it('stops polling once the server refuses the list and retries while it is unreachable', () => {
    vi.useFakeTimers();
    try {
      const refused = vi.fn(() => throwError(() => new ApiError('Evaluation not found', 404)));
      const unreachable = vi.fn(() => throwError(() => new ApiError('Bad Gateway', 502)));
      const offline = vi.fn(() => throwError(() => new ApiError('Connection to the server failed.', 0)));
      for (const forEvaluation of [refused, unreachable, offline]) {
        TestBed.resetTestingModule();
        TestBed.configureTestingModule({
          imports: [ExecJobListComponent],
          providers: [provideRouter([]), { provide: ExecJobsService, useValue: { forEvaluation } }],
        });
        const fixture = TestBed.createComponent(ExecJobListComponent);
        fixture.componentRef.setInput('evaluation', 'e1');
        fixture.componentRef.setInput('project', 'acme');
        fixture.componentRef.setInput('task', 'demo');
        fixture.detectChanges();
        vi.advanceTimersByTime(3 * 4000);
        fixture.destroy();
      }

      expect(refused).toHaveBeenCalledTimes(1);
      expect(unreachable).toHaveBeenCalledTimes(4);
      expect(offline).toHaveBeenCalledTimes(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it('renders nothing for an evaluation without exec jobs', () => {
    TestBed.configureTestingModule({
      imports: [ExecJobListComponent],
      providers: [
        provideRouter([]),
        { provide: ExecJobsService, useValue: { forEvaluation: () => of([]) } },
      ],
    });
    const fixture = TestBed.createComponent(ExecJobListComponent);
    fixture.componentRef.setInput('evaluation', 'e1');
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'demo');
    fixture.detectChanges();

    expect((fixture.nativeElement as HTMLElement).querySelector('.exec-jobs')).toBeNull();
  });

  describe('job actions', () => {
    function renderJob(
      status: ExecJobSummary['status'],
      canTrigger: boolean,
      dispatchedJob: string | null = null,
      confirmed = true,
    ) {
      const abort = vi.fn(() => of('Success'));
      const confirm = vi.fn((request: ConfirmRequest) => (confirmed ? request.action() : EMPTY));
      const job: ExecJobSummary = { ...queuedJob(), status, dispatched_job: dispatchedJob };
      TestBed.configureTestingModule({
        imports: [ExecJobListComponent],
        providers: [
          provideRouter([]),
          { provide: ExecJobsService, useValue: { forEvaluation: () => of([job]), abort } },
          { provide: ConfirmService, useValue: { confirm } },
        ],
      });
      const fixture = TestBed.createComponent(ExecJobListComponent);
      fixture.componentRef.setInput('evaluation', 'e1');
      fixture.componentRef.setInput('project', 'acme');
      fixture.componentRef.setInput('task', 'demo');
      fixture.componentRef.setInput('canTrigger', canTrigger);
      fixture.detectChanges();
      return { fixture, abort, confirm };
    }

    function openMenu(fixture: ReturnType<typeof renderJob>['fixture'], via: 'kebab' | 'contextmenu'): HTMLElement[] {
      const root = fixture.nativeElement as HTMLElement;
      if (via === 'kebab') (root.querySelector('.job .entry-kebab') as HTMLElement).click();
      else root.querySelector('.job')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
      fixture.detectChanges();
      return Array.from(document.querySelectorAll<HTMLElement>('.gr-menu__item'));
    }

    const labels = (items: HTMLElement[]) => items.map((item) =>
      (item.textContent ?? '').replace(item.querySelector('.gr-menu__icon')?.textContent ?? '', '').trim());

    it.each(['kebab', 'contextmenu'] as const)('offers the job, terminal and abort for a running job via %s', (via) => {
      const { fixture } = renderJob('running', true, 'dj1');
      expect(labels(openMenu(fixture, via))).toEqual(['Show job', 'Terminal', 'Abort']);
    });

    it('links a finished job to its assignment on the Job Board', () => {
      const { fixture } = renderJob('completed', true, 'dj1');
      expect(labels(openMenu(fixture, 'kebab'))).toEqual(['Show job']);
      const item = fixture.componentInstance.menuModel().find((i) => i.label === 'Show job');
      expect(String(item?.routerLink)).toBe('/board/jobs/dj1');
    });

    it('leaves out Show job until a worker picked the job up', () => {
      const { fixture } = renderJob('queued', true);
      expect(labels(openMenu(fixture, 'kebab'))).toEqual(['Abort']);
    });

    it('hides the menu of a job without any action for the visitor', () => {
      const { fixture } = renderJob('queued', false);
      expect((fixture.nativeElement as HTMLElement).querySelector('.job .entry-kebab')).toBeNull();
    });

    it('hides abort from a visitor who may not trigger', () => {
      const { fixture } = renderJob('running', false);
      expect(labels(openMenu(fixture, 'kebab'))).not.toContain('Abort');
    });

    it('asks first, naming the entry point, then aborts the job from the menu', () => {
      const { fixture, abort, confirm } = renderJob('running', true);
      const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
      openMenu(fixture, 'kebab').find((item) => item.textContent?.includes('Abort'))!.click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'apps.x86_64-linux.train' }));
      expect(abort).toHaveBeenCalledWith('j1');
      expect(success).toHaveBeenCalledWith('Exec job aborted');
    });

    it('sends nothing when the abort is cancelled', () => {
      const { fixture, abort, confirm } = renderJob('running', true, null, false);
      openMenu(fixture, 'kebab').find((item) => item.textContent?.includes('Abort'))!.click();
      expect(confirm).toHaveBeenCalled();
      expect(abort).not.toHaveBeenCalled();
    });
  });
});
