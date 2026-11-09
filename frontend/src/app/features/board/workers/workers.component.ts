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
import { firstLoad, requestFailedText } from '../first-load';
import { clockTime, formatMegabytes, formatPercent } from '@shared/text';
import { oneOf, queryParamSignal } from '@shared/routing';
import { BadgeComponent, LoadStateComponent, TabSwitchComponent, TableComponent } from '@gradient/ui/ui';
import { MetricChartComponent, SegmentedBarComponent, ratioSegments } from '@shared/ui';
import { BoardWorker, BoardFleetPoint, DeviceClass, WorkerLoad, WorkerLoadAverage } from '@core/models';
import { links } from '@shared/routing';

type LoadRange = 'now' | '24h' | '7d' | '30d';

const LOAD_RANGE_HOURS: Record<Exclude<LoadRange, 'now'>, number> = { '24h': 24, '7d': 24 * 7, '30d': 24 * 30 };

const busyPercent = (busy: number, capacity: number) => Math.round((busy / capacity) * 1000) / 10;

@Component({
  selector: 'app-board-workers',
  standalone: true,
  imports: [
    CommonModule, FormsModule, RouterModule, MetricChartComponent, TableComponent, LoadStateComponent, BadgeComponent,
    SegmentedBarComponent, TabSwitchComponent,
  ],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading workers..." errorTitle="Failed to load workers" (retry)="retry()">
      <gr-table class="workers">
        <thead>
          <tr><th>Worker</th><th>Projects</th><th>State</th><th>Jobs</th><th>CPU load</th><th>Free RAM</th><th>Arch</th>@if (anyWorkerHasExecLabelsOrDevices()) {<th>Exec</th>}</tr>
        </thead>
        <tbody>
          @for (w of workers() ?? []; track $index) {
            <tr>
              <td class="mono">{{ w.name ?? w.id ?? '-' }}</td>
              <td class="projects">
                @for (p of w.projects; track p.id; let last = $last) {
                  <a [routerLink]="links.project(p.name)">{{ p.display_name || p.name }}</a>@if (!last) {, }
                } @empty { - }
              </td>
              <td>{{ w.draining ? 'draining' : 'active' }}</td>
              <td>{{ w.assigned_jobs }}/{{ w.max_concurrent_builds }}</td>
              <td>{{ w.cpu_usage_pct != null ? percent(w.cpu_usage_pct) : '-' }}</td>
              <td>{{ megabytes(w.ram_free_mb) }}</td>
              <td class="mono">{{ w.architectures.join(', ') || '-' }}</td>
              @if (anyWorkerHasExecLabelsOrDevices()) {
              <td class="exec">
                @if (w.exec; as exec) {
                  <span class="labels">
                    @for (label of getExecLabelsWithoutWorkerId(w); track label) { <gr-badge>{{ label }}</gr-badge> }
                  </span>
                  @for (d of exec.devices; track d.name) {
                    <span class="device">
                      @if (d.total) { <gr-segmented-bar [segments]="heldSegments(d)" /> }
                      <span class="mono">{{ d.name }} {{ d.free }}/{{ d.total }} free</span>
                    </span>
                  }
                } @else { - }
              </td>
              }
            </tr>
          } @empty {
            <tr><td [attr.colspan]="anyWorkerHasExecLabelsOrDevices() ? 8 : 7" class="muted">{{ workers() === null ? failed : 'No connected workers.' }}</td></tr>
          }
        </tbody>
      </gr-table>

      <gr-metric-chart
        title="Fleet over time (connected vs draining)"
        doc="ui/job-board/#workers"
        type="area"
        [series]="fleetSeries()"
        [categories]="fleetCats()"
        [colors]="['--gr-chart-green', '--gr-chart-orange']"
        [error]="fleet() === null"
      ></gr-metric-chart>

      <gr-load-state [error]="averageError()" errorTitle="Failed to load worker load" (retry)="loadAverage()" />
      <div class="row">
        <gr-metric-chart
          title="Load by capability (busy %)"
          doc="ui/job-board/#workers"
          type="radar"
          [height]="300"
          [series]="capLoad().series"
          [categories]="capLoad().cats"
          [colors]="['--gr-chart-cyan']"
          [valueFormatter]="percent"
          [loading]="isAverageLoadForSelectedRangeLoading()"
          [error]="selectedRangeLoadFailed()"
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
          [colors]="['--gr-chart-green']"
          [valueFormatter]="percent"
          [loading]="isAverageLoadForSelectedRangeLoading()"
          [error]="selectedRangeLoadFailed()"
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
        [colors]="['--gr-chart-pink']"
        [valueFormatter]="percent"
        [error]="load() === null"
      ></gr-metric-chart>
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './workers.component.scss',
})
export class BoardWorkersComponent implements OnInit {
  protected readonly links = links;
  private board = inject(BoardService);
  protected first = firstLoad();
  protected readonly failed = requestFailedText;
  workers = signal<BoardWorker[] | null>(null);
  fleet = signal<BoardFleetPoint[] | null>(null);
  load = signal<WorkerLoad | null>(null);
  averageLoad = signal<WorkerLoadAverage | null>(null);
  averageError = signal<unknown>(null);
  readonly loadRanges: { label: string; value: LoadRange }[] = [
    { label: 'Now', value: 'now' },
    { label: '24h', value: '24h' },
    { label: '7d', value: '7d' },
    { label: '30d', value: '30d' },
  ];
  loadRange = queryParamSignal<LoadRange>('load', 'now', oneOf(this.loadRanges.map((r) => r.value)));
  private rangedLoad = computed(() => (this.loadRange() === 'now' ? this.load() : this.averageLoad()));
  protected selectedRangeLoadFailed = computed(() => (this.loadRange() === 'now' ? this.load() === null : this.averageError() !== null));
  protected isAverageLoadForSelectedRangeLoading = computed(() =>
    this.loadRange() !== 'now' && this.averageLoad() === null && this.averageError() === null);

  readonly megabytes = formatMegabytes;
  readonly heldSegments = (d: DeviceClass) => ratioSegments(d.total - d.free, d.total);
  readonly getExecLabelsWithoutWorkerId = (w: BoardWorker) => (w.exec?.labels ?? []).filter((label) => label !== w.id);
  readonly percent = (value: number) => formatPercent(value / 100);
  anyWorkerHasExecLabelsOrDevices = computed(() =>
    (this.workers() ?? []).some((w) => this.getExecLabelsWithoutWorkerId(w).length > 0 || (w.exec?.devices.length ?? 0) > 0));

  fleetCats = computed(() => (this.fleet() ?? []).map((p) => clockTime(p.bucket_start)));
  fleetSeries = computed(() => [
    { name: 'connected', data: (this.fleet() ?? []).map((p) => p.connected) },
    { name: 'draining', data: (this.fleet() ?? []).map((p) => p.draining) },
  ]);

  private radar(buckets: { key: string; in_flight: number; capacity: number }[]) {
    return {
      cats: buckets.map((b) => b.key),
      series: [
        {
          name: 'busy %',
          data: buckets.map((b) =>
            b.capacity > 0 ? busyPercent(b.in_flight, b.capacity) : 0
          ),
        },
      ],
    };
  }

  capLoad = computed(() => this.radar(this.rangedLoad()?.by_capability ?? []));
  archLoad = computed(() => this.radar(this.rangedLoad()?.by_architecture ?? []));
  featLoad = computed(() => this.radar(this.load()?.by_feature ?? []));

  ngOnInit(): void {
    this.loadAll();
  }

  retry(): void {
    this.first.restart();
    this.loadAll();
  }

  private loadAll(): void {
    this.board.getWorkers().pipe(this.first.track()).subscribe((w) => this.workers.set(w));
    this.board.getFleet(24).pipe(this.first.track()).subscribe((f) => this.fleet.set(f));
    this.board.getWorkerLoad().pipe(this.first.track()).subscribe((l) => this.load.set(l));
    this.loadAverage();
  }

  setLoadRange(range: LoadRange): void {
    this.loadRange.set(range);
    this.loadAverage();
  }

  loadAverage(): void {
    const range = this.loadRange();
    this.averageError.set(null);
    if (range === 'now') return;
    this.averageLoad.set(null);
    this.board.getWorkerLoadAverage(LOAD_RANGE_HOURS[range]).subscribe({
      next: (l) => this.averageLoad.set(l),
      error: (error: unknown) => this.averageError.set(error),
    });
  }
}
