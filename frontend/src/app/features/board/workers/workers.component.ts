/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import {
  BoardService,
  BoardWorker,
  BoardFleetPoint,
  LoadBucket,
  WorkerLoad,
} from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-workers',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <app-metric-chart
      title="Fleet over time (connected vs draining)"
      type="area"
      [series]="fleetSeries()"
      [categories]="fleetCats()"
      [colors]="['#28a745', '#fd7e14']"
    ></app-metric-chart>

    <app-metric-chart
      title="Capability over time"
      type="line"
      [series]="capOverTime()"
      [categories]="fleetCats()"
      [colors]="['#17a2b8', '#6f42c1', '#e83e8c']"
    ></app-metric-chart>

    <div class="row">
      <app-metric-chart
        title="Load by capability (busy %)"
        type="radar"
        [height]="300"
        [series]="capLoad().series"
        [categories]="capLoad().cats"
        [colors]="['#17a2b8']"
      ></app-metric-chart>
      <app-metric-chart
        title="Load by architecture (busy %)"
        type="radar"
        [height]="300"
        [series]="archLoad().series"
        [categories]="archLoad().cats"
        [colors]="['#28a745']"
      ></app-metric-chart>
      <app-metric-chart
        title="Load by feature (busy %)"
        type="radar"
        [height]="300"
        [series]="featLoad().series"
        [categories]="featLoad().cats"
        [colors]="['#e83e8c']"
      ></app-metric-chart>
      <app-metric-chart
        title="Slot utilisation per worker (%)"
        type="bar"
        [height]="300"
        [series]="utilSeries()"
        [categories]="workerCats()"
        [colors]="['#6f42c1']"
      ></app-metric-chart>
    </div>

    <table class="workers">
      <thead>
        <tr><th>Worker</th><th>Org</th><th>State</th><th>Load</th><th>CPU%</th><th>RAM free</th><th>Arch</th></tr>
      </thead>
      <tbody>
        @for (w of workers(); track $index) {
          <tr>
            <td class="mono">{{ w.id ?? '-' }}</td>
            <td class="mono">{{ w.organization ?? '-' }}</td>
            <td>{{ w.draining ? 'draining' : 'active' }}</td>
            <td>{{ w.assigned_jobs }}/{{ w.max_concurrent_builds }}</td>
            <td>{{ w.cpu_usage_pct !== null ? (w.cpu_usage_pct | number: '1.0-0') : '-' }}</td>
            <td>{{ w.ram_free_mb !== null ? w.ram_free_mb + ' MB' : '-' }}</td>
            <td class="mono">{{ w.architectures.join(', ') || '-' }}</td>
          </tr>
        } @empty {
          <tr><td colspan="7" class="muted">No connected workers.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      app-metric-chart { display: block; margin-bottom: 1rem; }
      .row { display: grid; grid-template-columns: repeat(auto-fit, minmax(360px, 1fr)); gap: 1rem; }
      table.workers { width: 100%; border-collapse: collapse; margin-top: 1rem; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .muted { color: #818181; }
    `,
  ],
})
export class BoardWorkersComponent implements OnInit {
  private board = inject(BoardService);
  workers = signal<BoardWorker[]>([]);
  fleet = signal<BoardFleetPoint[]>([]);
  load = signal<WorkerLoad | null>(null);

  fleetCats = computed(() => this.fleet().map((p) => p.bucket_start.slice(11, 16)));
  fleetSeries = computed(() => [
    { name: 'connected', data: this.fleet().map((p) => p.connected) },
    { name: 'draining', data: this.fleet().map((p) => p.draining) },
  ]);
  capOverTime = computed(() => [
    { name: 'eval', data: this.fleet().map((p) => p.eval) },
    { name: 'fetch', data: this.fleet().map((p) => p.fetch) },
    { name: 'build', data: this.fleet().map((p) => p.build) },
  ]);

  private radar(buckets: LoadBucket[]) {
    return {
      cats: buckets.map((b) => b.key),
      series: [
        {
          name: 'busy %',
          data: buckets.map((b) =>
            b.capacity > 0 ? Math.round((b.in_flight / b.capacity) * 100) : 0
          ),
        },
      ],
    };
  }

  capLoad = computed(() => this.radar(this.load()?.by_capability ?? []));
  archLoad = computed(() => this.radar(this.load()?.by_architecture ?? []));
  featLoad = computed(() => this.radar(this.load()?.by_feature ?? []));

  workerCats = computed(() =>
    this.workers().filter((w) => w.id !== null).map((w) => (w.id ?? '').slice(0, 12))
  );
  utilSeries = computed(() => [
    {
      name: 'utilisation',
      data: this.workers()
        .filter((w) => w.id !== null)
        .map((w) =>
          w.max_concurrent_builds > 0
            ? Math.round((w.assigned_jobs / w.max_concurrent_builds) * 100)
            : 0
        ),
    },
  ]);

  ngOnInit(): void {
    this.board.getWorkers().subscribe((w) => this.workers.set(w));
    this.board.getFleet(24).subscribe((f) => this.fleet.set(f));
    this.board.getWorkerLoad().subscribe((l) => this.load.set(l));
  }
}
