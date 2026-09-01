/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { WorkerMetricsComponent } from './worker-metrics.component';
import { WorkersService } from '@core/services/workers.service';
import { ProjectsService } from '@core/services/projects.service';

const WORKER_ID = 'a0000000-0000-0000-0000-000000000001';

function setup(displayName: string | null): ComponentFixture<WorkerMetricsComponent> {
  TestBed.configureTestingModule({
    imports: [WorkerMetricsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ project: 'testproject', workerId: WORKER_ID }) },
        },
      },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Test Project' }) } },
      {
        provide: WorkersService,
        useValue: {
          getWorkerMetrics: () =>
            of({
              worker_id: WORKER_ID,
              display_name: displayName,
              samples: [],
              connections: [],
              jobs_dispatched: 4,
            }),
        },
      },
    ],
  });
  return TestBed.createComponent(WorkerMetricsComponent);
}

async function settled(fixture: ComponentFixture<WorkerMetricsComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

function crumbs(fixture: ComponentFixture<WorkerMetricsComponent>): string[] {
  const nav = fixture.nativeElement.querySelector('nav.breadcrumb') as HTMLElement;
  return Array.from(nav.querySelectorAll('.breadcrumb-link, .breadcrumb-current')).map((el) =>
    (el.textContent ?? '').trim(),
  );
}

describe('WorkerMetricsComponent', () => {
  it('names the worker rather than repeating its id', async () => {
    const fixture = setup('builder-1');
    await settled(fixture);
    expect((fixture.nativeElement.querySelector('h1') as HTMLElement).textContent).toContain('builder-1');
  });

  it('keeps the id visible, since jobs and samples are recorded against it', async () => {
    const fixture = setup('builder-1');
    await settled(fixture);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain(WORKER_ID);
  });

  it('falls back to the id when nothing names the worker any more', async () => {
    const fixture = setup(null);
    await settled(fixture);
    expect((fixture.nativeElement.querySelector('h1') as HTMLElement).textContent).toContain(WORKER_ID);
  });

  it('walks back through breadcrumbs rather than a lone back arrow', async () => {
    const fixture = setup('builder-1');
    await settled(fixture);
    expect(crumbs(fixture)).toEqual(['Test Project', 'Workers', 'builder-1']);
    expect(fixture.nativeElement.querySelector('.back')).toBeNull();
  });
});
