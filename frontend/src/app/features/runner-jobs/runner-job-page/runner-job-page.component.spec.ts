/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';
import type { RunnerJobResponse, RunnerJobStatus } from '@core/models';
import { RunnerJobsService } from '@core/services/runner-jobs.service';
import { RunnerJobPageComponent } from './runner-job-page.component';

function job(status: RunnerJobStatus): RunnerJobResponse {
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
  };
}

function setup(status: RunnerJobStatus): { fixture: ComponentFixture<RunnerJobPageComponent>; abort: ReturnType<typeof vi.fn> } {
  const abort = vi.fn(() => of('Success'));
  const route = {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo', job: 'j1' }) },
    parent: { data: of({ taskAccess: { access: { managed: false, canEdit: true, canTrigger: true } } }) },
  } as unknown as ActivatedRoute;
  TestBed.configureTestingModule({
    imports: [RunnerJobPageComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: route },
      {
        provide: RunnerJobsService,
        useValue: { get: () => of(job(status)), log: () => of('hello'), abort },
      },
    ],
  });
  const fixture = TestBed.createComponent(RunnerJobPageComponent);
  fixture.detectChanges();
  return { fixture, abort };
}

function abortButton(root: HTMLElement): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) => (b.textContent ?? '').includes('Abort'));
}

describe('RunnerJobPageComponent', () => {
  it('offers no abort for a finished job', () => {
    const { fixture } = setup('completed');
    expect(abortButton(fixture.nativeElement)).toBeUndefined();
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
        imports: [RunnerJobPageComponent],
        providers: [
          provideRouter([]),
          { provide: ActivatedRoute, useValue: route },
          { provide: RunnerJobsService, useValue: { get, log: () => of('hello'), abort: vi.fn() } },
        ],
      });
      const fixture = TestBed.createComponent(RunnerJobPageComponent);
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
});
