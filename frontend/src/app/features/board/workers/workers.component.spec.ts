/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import type { WorkerLoad, WorkerLoadAverage } from '@core/models';
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

function setup() {
  const getWorkerLoadAverage = vi.fn((_hours: number) => of(AVERAGE));
  TestBed.configureTestingModule({
    imports: [BoardWorkersComponent],
    providers: [
      provideRouter([]),
      {
        provide: BoardService,
        useValue: {
          getWorkers: () => of([]),
          getFleet: () => of([]),
          getWorkerLoad: () => of(NOW),
          getWorkerLoadAverage,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardWorkersComponent);
  fixture.detectChanges();
  return { cmp: fixture.componentInstance, getWorkerLoadAverage };
}

describe('BoardWorkersComponent - load range', () => {
  it('shows the live load until a range is picked', () => {
    const { cmp, getWorkerLoadAverage } = setup();
    expect(getWorkerLoadAverage).not.toHaveBeenCalled();
    expect(cmp.capLoad().series[0].data).toEqual([0, 0, 100]);
  });

  it('averages both radar charts over the picked range', () => {
    const { cmp, getWorkerLoadAverage } = setup();
    cmp.setLoadRange('7d');
    expect(getWorkerLoadAverage).toHaveBeenCalledWith(168);
    expect(cmp.capLoad().series[0].data).toEqual([25, 0, 50]);
    expect(cmp.archLoad()).toEqual({ cats: ['x86_64-linux', 'aarch64-linux'], series: [{ name: 'busy %', data: [25, 25] }] });
  });
});
