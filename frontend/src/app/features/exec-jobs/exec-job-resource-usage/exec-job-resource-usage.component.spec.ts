/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { type Observable, Subject, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import type { ExecJobMetricsResponse } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { ExecJobResourceUsageComponent } from './exec-job-resource-usage.component';

const MIB = 1024 * 1024;
const EMPTY_TITLE = 'No metrics recorded for this attempt';

function createExecJobMetricsResponseFixture(overrides: Partial<ExecJobMetricsResponse> = {}): ExecJobMetricsResponse {
  return {
    attempt: 1,
    attempt_open: false,
    requested_cpus: 2,
    requested_memory_bytes: 500 * MIB,
    requested_disk_bytes: 2048 * MIB,
    usable_memory_bytes: 480 * MIB,
    usable_disk_bytes: 1946 * MIB,
    cpu_avg: 0.9,
    cpu_max: 1.8,
    ram_max_bytes: 412 * MIB,
    disk_max_bytes: 1126 * MIB,
    points: [
      { elapsed_ms: 0, cpu_avg: 0.5, cpu_max: 1.2, ram_avg_bytes: 200 * MIB, ram_max_bytes: 300 * MIB, disk_max_bytes: 1000 * MIB },
      { elapsed_ms: 3_725_000, cpu_avg: 1.3, cpu_max: 1.8, ram_avg_bytes: 400 * MIB, ram_max_bytes: 412 * MIB, disk_max_bytes: 1126 * MIB },
    ],
    ...overrides,
  };
}

function createComponentWithMockedExecJobsService(
  getJobAttemptMetrics: (job: string, attempt: number | null, node: number | null) => Observable<ExecJobMetricsResponse>,
) {
  const fetch = vi.fn(getJobAttemptMetrics);
  TestBed.configureTestingModule({
    imports: [ExecJobResourceUsageComponent],
    providers: [{ provide: ExecJobsService, useValue: { getJobAttemptMetrics: fetch } }],
  });
  const fixture = TestBed.createComponent(ExecJobResourceUsageComponent);
  fixture.componentRef.setInput('job', 'j1');
  fixture.componentRef.setInput('attempt', 1);
  fixture.componentRef.setInput('node', 0);
  fixture.detectChanges();
  return { fixture, fetch, root: fixture.nativeElement as HTMLElement };
}

describe('ExecJobResourceUsageComponent', () => {
  it('shows peaks against usable and requested size', () => {
    const { root, fetch } = createComponentWithMockedExecJobsService(() => of(createExecJobMetricsResponseFixture()));
    expect(fetch).toHaveBeenCalledWith('j1', 1, 0);
    const text = root.textContent ?? '';
    expect(text).toContain('1.8 / 2 CPUs');
    expect(text).toContain('412 MiB / 480 MiB usable (500 MiB requested)');
    expect(text).toContain('1.1 GiB / 1.9 GiB usable (2.0 GiB requested)');
    expect(text).toContain('90.0 % of usable capacity');
    expect(text).toContain('Average 0.9 CPUs');
    expect(root.querySelectorAll('gr-metric-chart')).toHaveLength(3);
  });

  it('labels the shared time axis with the time since program start', () => {
    const { fixture } = createComponentWithMockedExecJobsService(() => of(createExecJobMetricsResponseFixture()));
    expect(fixture.componentInstance.elapsedTimeAxisLabels()).toEqual(['0:00:00', '1:02:05']);
  });

  it('shows the empty state without points', () => {
    const { root } = createComponentWithMockedExecJobsService(() => of(createExecJobMetricsResponseFixture({ points: [] })));
    expect(root.textContent).toContain(EMPTY_TITLE);
    expect(root.querySelector('gr-metric-chart')).toBeNull();
  });

  it('shows the empty state for an attempt the server has no metrics for', () => {
    const { root } = createComponentWithMockedExecJobsService(() => throwError(() => new ApiError('Exec job attempt not found', 404)));
    expect(root.textContent).toContain(EMPTY_TITLE);
    expect(root.textContent).not.toContain('Failed to Load Metrics');
  });

  it('shows any other failure as an error, never as an attempt without metrics', () => {
    const { root } = createComponentWithMockedExecJobsService(() => throwError(() => new ApiError('database is down', 500)));
    expect(root.textContent).toContain('Failed to Load Metrics');
    expect(root.textContent).not.toContain(EMPTY_TITLE);
  });

  it('stops polling after the attempt ends', async () => {
    vi.useFakeTimers();
    try {
      const responses = [createExecJobMetricsResponseFixture({ attempt_open: true }), createExecJobMetricsResponseFixture({ attempt_open: false })];
      const { fetch } = createComponentWithMockedExecJobsService(() => of(responses.shift() ?? createExecJobMetricsResponseFixture()));
      expect(fetch).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(2000);
      expect(fetch).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(10_000);
      expect(fetch).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it('loads the newly selected attempt and node without showing the previous one', () => {
    const next = new Subject<ExecJobMetricsResponse>();
    const { fixture, fetch, root } = createComponentWithMockedExecJobsService((_job, attempt) =>
      attempt === 1 ? of(createExecJobMetricsResponseFixture()) : next);
    expect(root.textContent).toContain('1.8 / 2 CPUs');

    fixture.componentRef.setInput('attempt', 2);
    fixture.componentRef.setInput('node', 1);
    fixture.detectChanges();
    expect(fetch).toHaveBeenLastCalledWith('j1', 2, 1);
    expect(root.textContent).not.toContain('1.8 / 2 CPUs');

    next.next(createExecJobMetricsResponseFixture({ attempt: 2, cpu_max: 0.4 }));
    fixture.detectChanges();
    expect(root.textContent).toContain('0.4 / 2 CPUs');
  });
});
