/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { EntryPointMetricsComponent } from './entry-point-metrics.component';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';

const EMPTY_TITLE = 'No completed evaluations found for this entry point.';

function render(getEntryPointMetrics: () => Observable<unknown>) {
  const metrics = vi.fn(getEntryPointMetrics);
  TestBed.configureTestingModule({
    imports: [EntryPointMetricsComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: {
            paramMap: convertToParamMap({ project: 'acme', task: 'demo' }),
            queryParamMap: convertToParamMap({ eval: 'packages.x86_64-linux.hello' }),
          },
        },
      },
      { provide: TasksService, useValue: { getEntryPointMetrics: metrics, getTaskInfo: () => of({ display_name: 'Demo' }) } },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
    ],
  });
  const fixture = TestBed.createComponent(EntryPointMetricsComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, metrics, fixture };
}

describe('EntryPointMetricsComponent', () => {
  it('shows a failed load as an error, never as an entry point without history', () => {
    const { root } = render(() => throwError(() => new Error('database is down')));
    expect(root.textContent).toContain('Failed to load metrics for entry points');
    expect(root.textContent).not.toContain(EMPTY_TITLE);
  });

  it('loads the metrics again on Try Again with the same entry point', () => {
    const { root, metrics, fixture } = render(() => throwError(() => new Error('down')));
    metrics.mockImplementation(() => of({ points: [], keep_evaluations: 30 }));
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(metrics).toHaveBeenLastCalledWith('acme', 'demo', 'packages.x86_64-linux.hello');
    expect(root.textContent).toContain(EMPTY_TITLE);
  });
});
