/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import type { RunnerJobSummary } from '@core/models';
import { RunnerJobsService } from '@core/services/runner-jobs.service';
import { RunnerJobListComponent } from './runner-job-list.component';

function queuedJob(): RunnerJobSummary {
  return {
    id: 'j1',
    entry_point: 'apps.x86_64-linux.train',
    status: 'queued',
    waiting_reason: {
      kind: 'runner_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 2,
      matching_workers: 1,
    },
    nodes: 1,
    labels: ['lab-a'],
    devices: [{ name: 'gpu:h100', count: 8 }],
  };
}

describe('RunnerJobListComponent', () => {
  it('shows a queued job with its status icon, wait reason, labels and devices', () => {
    TestBed.configureTestingModule({
      imports: [RunnerJobListComponent],
      providers: [
        provideRouter([]),
        { provide: RunnerJobsService, useValue: { forEvaluation: () => of([queuedJob()]) } },
      ],
    });
    const fixture = TestBed.createComponent(RunnerJobListComponent);
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

  it('renders nothing for an evaluation without runner jobs', () => {
    TestBed.configureTestingModule({
      imports: [RunnerJobListComponent],
      providers: [
        provideRouter([]),
        { provide: RunnerJobsService, useValue: { forEvaluation: () => of([]) } },
      ],
    });
    const fixture = TestBed.createComponent(RunnerJobListComponent);
    fixture.componentRef.setInput('evaluation', 'e1');
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'demo');
    fixture.detectChanges();

    expect((fixture.nativeElement as HTMLElement).querySelector('.runner-jobs')).toBeNull();
  });
});
