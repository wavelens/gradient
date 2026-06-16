/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, BoardWorker, BoardFleetPoint } from '@core/services/board.service';
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
        [series]="loadRadar()"
        [categories]="['eval', 'fetch', 'build']"
        [colors]="['#17a2b8']"
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

  loadRadar = computed(() => {
    const w = this.workers();
    const busy = (pred: (x: BoardWorker) => boolean) => {
      const sel = w.filter(pred);
      const max = sel.reduce((a, x) => a + x.max_concurrent_builds, 0);
      const used = sel.reduce((a, x) => a + x.assigned_jobs, 0);
      return max > 0 ? Math.round((used / max) * 100) : 0;
    };
    return [{ name: 'busy %', data: [busy((x) => x.eval), busy((x) => x.fetch), busy((x) => x.build)] }];
  });

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
  }
}
