/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { WorkersService } from '@core/services/workers.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import {
  CardGridComponent,
  LoadStateComponent,
  PageLayoutComponent,
  StatCardComponent,
  TableComponent,
} from '@gradient/ui/ui';
import { MetricChartComponent } from '@shared/ui';
import { clockTime, formatMegabytes, formatPercent, formatQuantity, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { WorkerSamplePoint, WorkerConnectionEntry, WorkerMetricsResponse } from '@core/models';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { tap } from 'rxjs';
import { links } from '@shared/routing';

@Component({
  selector: 'app-worker-metrics',
  standalone: true,
  imports: [
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
    RouterModule,
    MetricChartComponent,
    PageLayoutComponent,
    CardGridComponent,
    LoadStateComponent,
    StatCardComponent,
    TableComponent,
  ],
  template: `
    <gr-page-layout
      [breadcrumb]="breadcrumb()"
      [title]="workerName()"
      subtitle="Live metrics, connection history and assigned jobs for this worker"
    >
      <span slot="meta" class="mono">{{ workerId }}</span>

      <gr-load-state [loading]="loading()" [error]="loadError()" [empty]="!loaded()" errorTitle="Failed to load worker stats" (retry)="load()">
      <gr-card-grid min="160px">
        <gr-stat-card label="Samples" [value]="samples().length" />
        <gr-stat-card label="Jobs assigned" [value]="jobsAssigned()" />
        <gr-stat-card label="Sessions" [value]="connections().length" />
      </gr-card-grid>

      <gr-card-grid min="380px">
        <gr-metric-chart title="CPU usage" type="line" [series]="cpuSeries()" [categories]="times()" [colors]="['--gr-chart-cyan']" [valueFormatter]="percent" [yAxisMax]="100"></gr-metric-chart>
        <gr-metric-chart title="RAM free" type="area" [series]="ramSeries()" [categories]="times()" [colors]="['--gr-chart-green']" [valueFormatter]="megabytes"></gr-metric-chart>
        <gr-metric-chart title="Upload and download speed" type="line" [series]="transferSeries()" [categories]="times()" [colors]="['--gr-chart-purple', '--gr-chart-teal']" [valueFormatter]="mbps"></gr-metric-chart>
        <gr-metric-chart title="Disk speed" type="line" [series]="diskSeries()" [categories]="times()" [colors]="['--gr-chart-orange']" [valueFormatter]="mbps"></gr-metric-chart>
        <gr-metric-chart title="Assigned jobs" type="area" [series]="loadSeries()" [categories]="times()" [colors]="['--gr-chart-pink']"></gr-metric-chart>
      </gr-card-grid>

      <h2>Connection history</h2>
      <gr-table>
        <thead><tr><th>Connected</th><th>Disconnected</th></tr></thead>
        <tbody>
          @for (c of connections(); track $index) {
            <tr><td>{{ c.connected_at | serverDate: 'short' }}</td><td>{{ c.disconnected_at ? (c.disconnected_at | serverDate: 'short') : 'connected' }}</td></tr>
          } @empty {
            <tr><td colspan="2" class="muted">No sessions recorded.</td></tr>
          }
        </tbody>
      </gr-table>
      </gr-load-state>
    </gr-page-layout>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './worker-metrics.component.scss',
})
export class WorkerMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workers = inject(WorkersService);
  private crumbs = inject(BreadcrumbsService);

  project = '';
  workerId = '';
  displayName = signal<string | null>(null);
  samples = signal<WorkerSamplePoint[]>([]);
  connections = signal<WorkerConnectionEntry[]>([]);
  jobsAssigned = signal(0);
  loading = signal(true);
  loaded = signal(false);
  loadError = signal<unknown>(null);

  /// The id is the last resort: history outlives the registration that named it.
  workerName = computed(() => this.displayName() || this.workerId);
  breadcrumb = computed(() => this.crumbs.projectSettings(
    this.project,
    { label: 'Workers', link: links.projectPage(this.project, 'workers') },
    { label: this.workerName() },
  ));

  readonly percent = (value: number) => formatPercent(value / 100);
  readonly megabytes = formatMegabytes;
  readonly mbps = (value: number) => formatQuantity(value, 'Mbps');

  times = computed(() => this.samples().map((s) => clockTime(s.at)));
  cpuSeries = computed(() => [{ name: 'cpu', data: this.samples().map((s) => s.cpu_usage_pct ?? 0) }]);
  ramSeries = computed(() => [{ name: 'ram free', data: this.samples().map((s) => s.ram_free_mb ?? 0) }]);
  transferSeries = computed(() => [
    { name: 'upload', data: this.samples().map((s) => s.upload_speed_mbps ?? 0) },
    { name: 'download', data: this.samples().map((s) => s.download_speed_mbps ?? 0) },
  ]);
  diskSeries = computed(() => [{ name: 'disk', data: this.samples().map((s) => s.disk_speed_mbps ?? 0) }]);
  loadSeries = computed(() => [{ name: 'assigned', data: this.samples().map((s) => s.assigned_jobs) }]);

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      this.workers.getWorkerMetrics(this.project, this.workerId).pipe(
        tap((stats) => this.copyWorkerMetricsResponseToSignalsAndMarkLoaded(stats))));
  }

  ngOnInit(): void {
    this.project = this.route.snapshot.paramMap.get('project') ?? '';
    this.workerId = this.route.snapshot.paramMap.get('workerId') ?? '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.workers.getWorkerMetrics(this.project, this.workerId).subscribe({
      next: (stats) => {
        this.copyWorkerMetricsResponseToSignalsAndMarkLoaded(stats);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private copyWorkerMetricsResponseToSignalsAndMarkLoaded(stats: WorkerMetricsResponse): void {
    this.displayName.set(stats.display_name);
    this.samples.set(stats.samples);
    this.connections.set(stats.connections);
    this.jobsAssigned.set(stats.jobs_dispatched);
    this.loaded.set(true);
    this.loadError.set(null);
  }
}
