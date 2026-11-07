/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { TaskMetricsComponent } from './task-metrics.component';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';

function render(getTaskMetrics: () => Observable<unknown>) {
  const metrics = vi.fn(getTaskMetrics);
  TestBed.configureTestingModule({
    imports: [TaskMetricsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }) } } },
      { provide: TasksService, useValue: { getTaskMetrics: metrics, getTaskInfo: () => of({ display_name: 'Demo' }) } },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
    ],
  });
  const fixture = TestBed.createComponent(TaskMetricsComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, metrics, fixture };
}

describe('TaskMetricsComponent', () => {
  it('says so when no evaluation has completed yet', () => {
    expect(render(() => of({ points: [], keep_evaluations: 30 })).root.textContent).toContain('No completed evaluations yet.');
  });

  it('shows a failed load as an error, never as "no completed evaluations"', () => {
    const { root } = render(() => throwError(() => new Error('database is down')));
    expect(root.textContent).toContain('Failed to load task metrics');
    expect(root.textContent).not.toContain('No completed evaluations yet.');
  });

  it('loads the metrics again on Try Again', () => {
    const { root, metrics, fixture } = render(() => throwError(() => new Error('down')));
    metrics.mockImplementation(() => of({ points: [], keep_evaluations: 30 }));
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(metrics).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('No completed evaluations yet.');
  });
});
