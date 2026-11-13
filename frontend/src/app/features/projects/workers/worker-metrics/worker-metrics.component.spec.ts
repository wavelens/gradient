/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { Observable, of, throwError } from 'rxjs';
import { WorkerMetricsComponent } from './worker-metrics.component';
import { WorkersService } from '@core/services/workers.service';
import { ProjectsService } from '@core/services/projects.service';

const WORKER_ID = 'a0000000-0000-0000-0000-000000000001';

const metrics = (displayName: string) =>
  of({ worker_id: WORKER_ID, display_name: displayName, samples: [], connections: [], jobs_dispatched: 4 });

function setup(displayName: string, load: () => Observable<unknown> = () => metrics(displayName)): ComponentFixture<WorkerMetricsComponent> {
  TestBed.configureTestingModule({
    imports: [WorkerMetricsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ project: 'testproject', workerId: WORKER_ID }), data: {} },
          data: of({}),
        },
      },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Test Project' }) } },
      {
        provide: WorkersService,
        useValue: { getWorkerMetrics: vi.fn(load) },
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

  it('leaves the id in the URL only and marks open sessions with a dash', async () => {
    const fixture = setup('builder-1', () => of({
      worker_id: WORKER_ID, display_name: 'builder-1', samples: [], jobs_dispatched: 0,
      connections: [{ connected_at: '2026-11-08T10:00:00', disconnected_at: null }],
    }));
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).not.toContain(WORKER_ID);
    expect(fixture.nativeElement.querySelector('tbody td:last-child').textContent.trim()).toBe('-');
  });

  it('hides charts without reported values and keeps missing samples as gaps', async () => {
    const fixture = setup('builder-1', () => of({
      worker_id: WORKER_ID, display_name: 'builder-1', connections: [], jobs_dispatched: 0,
      samples: [
        { at: '2026-11-08T10:00:00', cpu_usage_pct: 40, assigned_jobs: 1 },
        { at: '2026-11-08T10:01:00', cpu_usage_pct: null, assigned_jobs: 0 },
      ],
    }));
    await settled(fixture);
    const charts = fixture.componentInstance.charts();
    expect(charts.map((chart) => chart.title)).toEqual(['CPU usage', 'Assigned jobs']);
    expect(charts[0].series[0].data).toEqual([40, null]);
  });

  it('names the speed chart after both series when both have values', async () => {
    const fixture = setup('builder-1', () => of({
      worker_id: WORKER_ID, display_name: 'builder-1', connections: [], jobs_dispatched: 0,
      samples: [{ at: '2026-11-08T10:00:00', upload_speed_mbps: 5, download_speed_mbps: 7 }],
    }));
    await settled(fixture);
    expect(fixture.componentInstance.charts().map((chart) => chart.title)).toContain('Upload and download speed');
  });

  it('leaves a series without values out of its chart, legend and title', async () => {
    const fixture = setup('builder-1', () => of({
      worker_id: WORKER_ID, display_name: 'builder-1', connections: [], jobs_dispatched: 0,
      samples: [{ at: '2026-11-08T10:00:00', upload_speed_mbps: 5, download_speed_mbps: null }],
    }));
    await settled(fixture);
    const speed = fixture.componentInstance.charts().find((chart) => chart.title === 'Upload speed')!;
    expect(speed.series.map((series) => series.name)).toEqual(['upload']);
    expect(speed.colors).toEqual(['--gr-chart-purple']);
  });

  it('walks back through breadcrumbs rather than a lone back arrow', async () => {
    const fixture = setup('builder-1');
    await settled(fixture);
    expect(crumbs(fixture)).toEqual(['Projects', 'Test Project', 'Settings', 'Workers']);
    expect(fixture.nativeElement.querySelector('.back')).toBeNull();
  });

  it('shows a failed load as an error instead of zero-filled charts', async () => {
    const fixture = setup('builder-1', () => throwError(() => new Error('worker unknown')));
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Failed to load worker stats');
    expect(text).toContain('worker unknown');
    expect(fixture.nativeElement.querySelector('gr-stat-card')).toBeNull();
  });

  it('loads the stats again on Try Again', async () => {
    let calls = 0;
    const fixture = setup('builder-1', () => (++calls === 1 ? throwError(() => new Error('down')) : metrics('builder-1')));
    await settled(fixture);
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    await settled(fixture);
    expect(calls).toBe(2);
    expect(fixture.nativeElement.querySelector('gr-empty-state')).not.toBeNull();
  });

  it('shows one empty state instead of empty charts and zero tiles when no samples exist', async () => {
    const fixture = setup('builder-1');
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelectorAll('gr-empty-state').length).toBe(1);
    expect(root.querySelector('gr-metric-chart')).toBeNull();
    expect(root.querySelector('gr-stat-card')).toBeNull();
  });

  it('keeps the metrics live by reloading them while the page is open', () => {
    vi.useFakeTimers();
    let jobs = 4;
    const fixture = setup('builder-1', () => of({ worker_id: WORKER_ID, display_name: 'builder-1', samples: [], connections: [], jobs_dispatched: jobs }));
    fixture.detectChanges();
    jobs = 5;
    vi.advanceTimersByTime(15_000);
    expect(fixture.componentInstance.jobsAssigned()).toBe(5);
    vi.useRealTimers();
  });
});
