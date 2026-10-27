/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { vi } from 'vitest';
import type { ExecJobSummary } from '@core/models';
import { ExecJobsService } from '@core/services/exec-jobs.service';
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
    fixture.detectChanges();

    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelector('gr-status-icon')?.getAttribute('data-phase')).toBe('queued');
    expect(host.textContent).toContain('waiting for free devices on 1 worker');
    expect(host.textContent).toContain('lab-a');
    expect(host.textContent).toContain('gpu:h100:8');
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
      fixture.componentRef.setInput('evaluationRunning', true);
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
});
