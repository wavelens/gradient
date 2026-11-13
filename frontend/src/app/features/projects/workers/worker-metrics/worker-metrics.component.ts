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
  EmptyStateComponent,
  LoadStateComponent,
  PageLayoutComponent,
  StatCardComponent,
  TableComponent,
} from '@gradient/ui/ui';
import { MetricChartComponent, type MetricChartType } from '@shared/ui';
import { clockTime, formatMebibytes, formatPercent, formatQuantity, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { WorkerSamplePoint, WorkerConnectionEntry, WorkerMetricsResponse } from '@core/models';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { tap } from 'rxjs';
import { links } from '@shared/routing';
import { ProjectSettingsNavigationComponent } from '../../project-settings-navigation/project-settings-navigation.component';

interface WorkerMetricChart {
  title: string;
  type: MetricChartType;
  series: { name: string; data: (number | null)[] }[];
  colors: string[];
  valueFormatter?: (value: number) => string;
  yAxisMax?: number;
}

interface WorkerMetricChartDefinition extends Omit<WorkerMetricChart, 'title'> {
  title: string | ((shownSeriesNames: string[]) => string);
}

function speedTitle(shownSeriesNames: string[]): string {
  const names = shownSeriesNames.join(' and ');
  return `${names.charAt(0).toUpperCase()}${names.slice(1)} speed`;
}

@Component({
  selector: 'app-worker-metrics',
  standalone: true,
  imports: [
    ProjectSettingsNavigationComponent,
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
    RouterModule,
    MetricChartComponent,
    PageLayoutComponent,
    CardGridComponent,
    EmptyStateComponent,
    LoadStateComponent,
    StatCardComponent,
    TableComponent,
  ],
  template: `
    <gr-page-layout
      [breadcrumb]="breadcrumb()"
      [title]="workerName()"
    >
      <app-project-settings-navigation slot="settings-navigation" [projectUrlName]="project" />
      <gr-load-state [loading]="loading()" [error]="loadError()" [empty]="!loaded()" errorTitle="Failed to load worker stats" (retry)="load()">
      @if (samples().length === 0) {
        <gr-empty-state icon="monitoring" title="No metrics yet" message="Connected workers send CPU, memory, network, and disk samples. Graphs appear with the first sample." />
      } @else {
        <gr-card-grid min="160px">
          <gr-stat-card label="Jobs for this project" valueExplanation="Jobs for this project that the scheduler assigned to this worker, running and finished alike. These records make up the number and stay on the server for 90 days by default." [value]="jobsAssigned()" />
          <gr-stat-card label="Sessions" valueExplanation="Connections this worker opened to the server, for all projects. Covers the connection records the server keeps, 90 days by default, and at most the 100 most recent." [value]="connections().length" />
        </gr-card-grid>

        <gr-card-grid min="380px">
          @for (chart of charts(); track chart.title) {
            <gr-metric-chart [title]="chart.title" [type]="chart.type" [series]="chart.series" [categories]="times()" [colors]="chart.colors"
              [valueFormatter]="chart.valueFormatter" [yAxisMax]="chart.yAxisMax"></gr-metric-chart>
          }
        </gr-card-grid>
      }

      <h2>Connection history</h2>
      <gr-table>
        <thead><tr><th>Connected</th><th>Disconnected</th></tr></thead>
        <tbody>
          @for (c of connections(); track $index) {
            <tr><td>{{ c.connected_at | serverDate }}</td><td>{{ c.disconnected_at ? (c.disconnected_at | serverDate) : '-' }}</td></tr>
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
  ));

  readonly percent = (value: number) => formatPercent(value / 100);
  readonly megabytes = formatMebibytes;
  readonly mbps = (value: number) => formatQuantity(value, 'Mbps');

  times = computed(() => this.samples().map((s) => clockTime(s.at)));
  charts = computed(() => {
    const values = (pick: (sample: WorkerSamplePoint) => number | null | undefined) => this.samples().map((s) => pick(s) ?? null);
    const charts: WorkerMetricChartDefinition[] = [
      { title: 'CPU usage', type: 'line', series: [{ name: 'cpu', data: values((s) => s.cpu_usage_pct) }], colors: ['--gr-chart-cyan'], valueFormatter: this.percent, yAxisMax: 100 },
      { title: 'RAM free', type: 'area', series: [{ name: 'ram free', data: values((s) => s.ram_free_mb) }], colors: ['--gr-chart-green'], valueFormatter: this.megabytes },
      {
        title: speedTitle,
        type: 'line',
        series: [
          { name: 'upload', data: values((s) => s.upload_speed_mbps) },
          { name: 'download', data: values((s) => s.download_speed_mbps) },
        ],
        colors: ['--gr-chart-purple', '--gr-chart-teal'],
        valueFormatter: this.mbps,
      },
      { title: 'Disk speed', type: 'line', series: [{ name: 'disk', data: values((s) => s.disk_speed_mbps) }], colors: ['--gr-chart-orange'], valueFormatter: this.mbps },
      { title: 'Assigned jobs', type: 'area', series: [{ name: 'assigned', data: values((s) => s.assigned_jobs) }], colors: ['--gr-chart-pink'] },
    ];
    const hasData = (series: WorkerMetricChart['series'][number]) => series.data.some((value) => value !== null);
    return charts
      .map((chart): WorkerMetricChart => {
        const series = chart.series.filter(hasData);
        return {
          ...chart,
          title: typeof chart.title === 'string' ? chart.title : chart.title(series.map((s) => s.name)),
          series,
          colors: chart.colors.filter((_, index) => hasData(chart.series[index])),
        };
      })
      .filter((chart) => chart.series.length > 0);
  });

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
