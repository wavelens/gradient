/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import type { BoardWorker, WorkerLoad, WorkerLoadAverage } from '@core/models';
import { BoardWorkersComponent } from './workers.component';

const bucket = (key: string, in_flight: number, capacity: number) => ({ key, in_flight, capacity, workers: 1 });

const NOW: WorkerLoad = {
  by_capability: [bucket('eval', 0, 4), bucket('network', 0, 4), bucket('build', 4, 4)],
  by_architecture: [bucket('x86_64-linux', 4, 4)],
  by_feature: [],
};

const AVERAGE: WorkerLoadAverage = {
  by_capability: [bucket('eval', 1, 4), bucket('network', 0, 4), bucket('build', 2, 4)],
  by_architecture: [bucket('x86_64-linux', 1, 4), bucket('aarch64-linux', 0.5, 2)],
};

function setup(
  average: () => Observable<WorkerLoadAverage> = () => of(AVERAGE),
  workers: Observable<BoardWorker[]> = of([]),
) {
  const getWorkerLoadAverage = vi.fn((_hours: number) => average());
  TestBed.configureTestingModule({
    imports: [BoardWorkersComponent],
    providers: [
      provideRouter([]),
      {
        provide: BoardService,
        useValue: {
          getWorkers: () => workers,
          getFleet: () => of([]),
          getWorkerLoad: () => of(NOW),
          getWorkerLoadAverage,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardWorkersComponent);
  fixture.detectChanges();
  return { cmp: fixture.componentInstance, getWorkerLoadAverage, fixture };
}

describe('BoardWorkersComponent - load range', () => {
  it('shows the live load until a range is picked', () => {
    const { cmp, getWorkerLoadAverage } = setup();
    expect(getWorkerLoadAverage).not.toHaveBeenCalled();
    expect(cmp.capLoad().series[0].data).toEqual([0, 0, 100]);
  });

  it('keeps a tenth of a percent so a light load does not read as zero', () => {
    const { cmp } = setup();
    cmp.load.set({ ...NOW, by_capability: [bucket('build', 1, 300)] });
    expect(cmp.capLoad().series[0].data).toEqual([0.3]);
  });

  it('shows a feature no worker offers as no data instead of an idle 0 %', () => {
    const { cmp, fixture } = setup();
    cmp.load.set({ ...NOW, by_feature: [bucket('kvm', 0, 0), bucket('benchmark', 0, 0)] });
    fixture.detectChanges();
    expect(cmp.featLoad().series[0].data).toEqual([null, null]);
    const chart = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-metric-chart')).at(-1)!;
    expect(chart.textContent).toContain('No data');
  });

  it('says the average failed instead of drawing empty radars like an idle fleet', () => {
    const { cmp, fixture } = setup(() => throwError(() => new Error('Internal server error')));
    cmp.setLoadRange('24h');
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load worker load');
    expect(root.textContent).toContain('Internal server error');

    cmp.setLoadRange('now');
    fixture.detectChanges();
    expect(root.textContent).not.toContain('Failed to load worker load');
  });

  it('averages both radar charts over the picked range', () => {
    const { cmp, getWorkerLoadAverage } = setup();
    cmp.setLoadRange('7d');
    expect(getWorkerLoadAverage).toHaveBeenCalledWith(168);
    expect(cmp.capLoad().series[0].data).toEqual([25, 0, 50]);
    expect(cmp.archLoad()).toEqual({ cats: ['x86_64-linux', 'aarch64-linux'], series: [{ name: 'busy %', data: [25, 25] }] });
  });
});

describe('BoardWorkersComponent - failed workers request', () => {
  it('says the worker list failed instead of claiming no workers are connected', () => {
    const { fixture } = setup(undefined, throwError(() => new Error('Internal server error')));
    const table = (fixture.nativeElement as HTMLElement).querySelector('.workers tbody')?.textContent ?? '';
    expect(table).toContain("Couldn't load");
    expect(table).not.toContain('No connected workers.');
  });

  it('says no workers are connected once the empty list loads', () => {
    const { fixture } = setup();
    expect((fixture.nativeElement as HTMLElement).querySelector('.workers tbody')?.textContent).toContain('No connected workers.');
  });
});

describe('BoardWorkersComponent - exec column', () => {
  const WORKER: BoardWorker = {
    id: 'w1', name: 'Dev Worker', projects: [], draining: false, assigned_jobs: 0, max_concurrent_builds: 4,
    eval: true, network: true, build: true, architectures: ['x86_64-linux'],
    exec: { labels: ['w1'], devices: [{ name: 'gpu', total: 0, free: 0 }, { name: 'fpga', total: 2, free: 1 }] },
  };

  it('leaves out the label every worker carries for its own id', () => {
    const { fixture } = setup(undefined, of([{ ...WORKER, exec: { ...WORKER.exec!, labels: ['w1', 'lab-a'] } }]));
    const labels = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('td.exec gr-badge')).map((b) => b.textContent?.trim());
    expect(labels).toEqual(['lab-a']);
  });

  it('draws a usage bar only for a device class the worker has', () => {
    const { fixture } = setup(undefined, of([WORKER]));
    const devices = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('td.exec .device'));
    expect(devices.map((d) => !!d.querySelector('gr-segmented-bar'))).toEqual([false, true]);
  });

  it('hides the column when no worker has a label beyond its own id or a device', () => {
    const { fixture } = setup(undefined, of([{ ...WORKER, exec: { labels: ['w1'], devices: [] } }]));
    const el = fixture.nativeElement as HTMLElement;
    expect(Array.from(el.querySelectorAll('thead th')).map((th) => th.textContent?.trim())).not.toContain('Exec');
    expect(el.querySelector('td.exec')).toBeNull();
  });
});
