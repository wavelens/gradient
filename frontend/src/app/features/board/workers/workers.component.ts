/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { RouterModule } from '@angular/router';
import { BoardService } from '@core/services/board.service';
import { firstLoad } from '../first-load';
import { clockTime, formatMegabytes, formatPercent } from '@shared/text';
import { oneOf, queryParamSignal } from '@shared/routing';
import { BadgeComponent, LoadingSpinnerComponent, TabSwitchComponent, TableComponent } from '@gradient/ui/ui';
import { MetricChartComponent, SegmentedBarComponent, ratioSegments } from '@shared/ui';
import { workerAxisLabel } from '../worker-label';
import { BoardWorker, BoardFleetPoint, DeviceClass, WorkerLoad, WorkerLoadAverage } from '@core/models';

type LoadRange = 'now' | '24h' | '7d' | '30d';

const LOAD_RANGE_HOURS: Record<Exclude<LoadRange, 'now'>, number> = { '24h': 24, '7d': 24 * 7, '30d': 24 * 30 };

@Component({
  selector: 'app-board-workers',
  standalone: true,
  imports: [
    CommonModule, FormsModule, RouterModule, MetricChartComponent, TableComponent, LoadingSpinnerComponent, BadgeComponent,
    SegmentedBarComponent, TabSwitchComponent,
  ],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading workers..." />
    } @else {
      <gr-metric-chart
        title="Fleet over time (connected vs draining)"
        doc="ui/job-board/#workers"
        type="area"
        [series]="fleetSeries()"
        [categories]="fleetCats()"
        [colors]="['#28a745', '#fd7e14']"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Capability over time"
        doc="ui/job-board/#workers"
        type="line"
        [series]="capOverTime()"
        [categories]="fleetCats()"
        [colors]="['#17a2b8', '#6f42c1', '#e83e8c']"
      ></gr-metric-chart>

      <div class="row">
        <gr-metric-chart
          title="Load by capability (busy %)"
          doc="ui/job-board/#workers"
          type="radar"
          [height]="300"
          [series]="capLoad().series"
          [categories]="capLoad().cats"
          [colors]="['#17a2b8']"
          [valueFormatter]="percent"
        >
          <gr-tab-switch slot="actions" ariaLabel="Capability load range" [options]="loadRanges" optionValue="value"
                         [ngModel]="loadRange()" (ngModelChange)="setLoadRange($event)" />
        </gr-metric-chart>
        <gr-metric-chart
          title="Load by architecture (busy %)"
          doc="ui/job-board/#workers"
          type="radar"
          [height]="300"
          [series]="archLoad().series"
          [categories]="archLoad().cats"
          [colors]="['#28a745']"
          [valueFormatter]="percent"
        >
          <gr-tab-switch slot="actions" ariaLabel="Architecture load range" [options]="loadRanges" optionValue="value"
                         [ngModel]="loadRange()" (ngModelChange)="setLoadRange($event)" />
        </gr-metric-chart>
      </div>

      <gr-metric-chart
        title="Load by feature (busy %)"
        doc="ui/job-board/#workers"
        type="bar"
        [height]="300"
        [series]="featLoad().series"
        [categories]="featLoad().cats"
        [colors]="['#e83e8c']"
        [valueFormatter]="percent"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Slot utilisation per worker (%)"
        doc="ui/job-board/#workers"
        type="bar"
        [height]="300"
        [series]="utilSeries()"
        [categories]="workerCats()"
        [colors]="['#6f42c1']"
        [valueFormatter]="percent"
      ></gr-metric-chart>

      <gr-table class="workers">
        <thead>
          <tr><th>Worker</th><th>Projects</th><th>State</th><th>Load</th><th>CPU</th><th>RAM free</th><th>Arch</th><th>Exec</th></tr>
        </thead>
        <tbody>
          @for (w of workers(); track $index) {
            <tr>
              <td class="mono">{{ w.name ?? w.id ?? '-' }}</td>
              <td class="projects">
                @for (p of w.projects; track p.id; let last = $last) {
                  <a [routerLink]="['/project', p.name]">{{ p.display_name || p.name }}</a>@if (!last) {, }
                } @empty { - }
              </td>
              <td>{{ w.draining ? 'draining' : 'active' }}</td>
              <td>{{ w.assigned_jobs }}/{{ w.max_concurrent_builds }}</td>
              <td>{{ w.cpu_usage_pct != null ? percent(w.cpu_usage_pct) : '-' }}</td>
              <td>{{ megabytes(w.ram_free_mb) }}</td>
              <td class="mono">{{ w.architectures.join(', ') || '-' }}</td>
              <td class="exec">
                @if (w.exec; as exec) {
                  <span class="labels">
                    @for (label of exec.labels; track label) { <gr-badge>{{ label }}</gr-badge> }
                  </span>
                  @for (d of exec.devices; track d.name) {
                    <span class="device">
                      <gr-segmented-bar [segments]="heldSegments(d)" />
                      <span class="mono">{{ d.name }} {{ d.free }}/{{ d.total }} free</span>
                    </span>
                  }
                } @else { - }
              </td>
            </tr>
          } @empty {
            <tr><td colspan="8" class="muted">No connected workers.</td></tr>
          }
        </tbody>
      </gr-table>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './workers.component.scss',
})
export class BoardWorkersComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();
  workers = signal<BoardWorker[]>([]);
  fleet = signal<BoardFleetPoint[]>([]);
  load = signal<WorkerLoad | null>(null);
  averageLoad = signal<WorkerLoadAverage | null>(null);
  readonly loadRanges: { label: string; value: LoadRange }[] = [
    { label: 'Now', value: 'now' },
    { label: '24h', value: '24h' },
    { label: '7d', value: '7d' },
    { label: '30d', value: '30d' },
  ];
  loadRange = queryParamSignal<LoadRange>('load', 'now', oneOf(this.loadRanges.map((r) => r.value)));
  private rangedLoad = computed(() => (this.loadRange() === 'now' ? this.load() : this.averageLoad()));

  readonly megabytes = formatMegabytes;
  readonly heldSegments = (d: DeviceClass) => ratioSegments(d.total - d.free, d.total);
  readonly percent = (value: number) => formatPercent(value / 100);

  fleetCats = computed(() => this.fleet().map((p) => clockTime(p.bucket_start)));
  fleetSeries = computed(() => [
    { name: 'connected', data: this.fleet().map((p) => p.connected) },
    { name: 'draining', data: this.fleet().map((p) => p.draining) },
  ]);
  capOverTime = computed(() => [
    { name: 'eval', data: this.fleet().map((p) => p.eval) },
    { name: 'network', data: this.fleet().map((p) => p.network) },
    { name: 'build', data: this.fleet().map((p) => p.build) },
  ]);

  private radar(buckets: { key: string; in_flight: number; capacity: number }[]) {
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

  capLoad = computed(() => this.radar(this.rangedLoad()?.by_capability ?? []));
  archLoad = computed(() => this.radar(this.rangedLoad()?.by_architecture ?? []));
  featLoad = computed(() => this.radar(this.load()?.by_feature ?? []));

  workerCats = computed(() =>
    this.workers().filter((w) => w.id !== null).map((w) => workerAxisLabel(w.name, w.id))
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
    this.board.getWorkers().pipe(this.first.track()).subscribe((w) => this.workers.set(w));
    this.board.getFleet(24).pipe(this.first.track()).subscribe((f) => this.fleet.set(f));
    this.board.getWorkerLoad().pipe(this.first.track()).subscribe((l) => this.load.set(l));
    this.loadAverage();
  }

  setLoadRange(range: LoadRange): void {
    this.loadRange.set(range);
    this.loadAverage();
  }

  private loadAverage(): void {
    const range = this.loadRange();
    if (range === 'now') return;
    this.averageLoad.set(null);
    this.board.getWorkerLoadAverage(LOAD_RANGE_HOURS[range]).subscribe((l) => this.averageLoad.set(l));
  }
}
