/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { WorkersService, WorkerSamplePoint, WorkerConnectionEntry } from '@core/services/workers.service';
import { ProjectsService } from '@core/services/projects.service';
import {
  CardGridComponent,
  MetricChartComponent,
  PageLayoutComponent,
  StatCardComponent,
  TableComponent,
} from '@shared/ui';

@Component({
  selector: 'app-worker-metrics',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    MetricChartComponent,
    PageLayoutComponent,
    CardGridComponent,
    StatCardComponent,
    TableComponent,
  ],
  template: `
    <gr-page-layout
      [breadcrumb]="[
        { label: projectDisplayName() || project, link: ['/project', project] },
        { label: 'Workers', link: ['/project', project, 'workers'] },
        { label: workerName() }
      ]"
      [title]="workerName()"
      subtitle="Live metrics, connection history and dispatched jobs for this worker"
    >
      <span slot="meta" class="mono">{{ workerId }}</span>

      <gr-card-grid min="160px">
        <gr-stat-card label="Samples" [value]="samples().length" />
        <gr-stat-card label="Jobs dispatched" [value]="jobsDispatched()" />
        <gr-stat-card label="Sessions" [value]="connections().length" />
      </gr-card-grid>

      <gr-card-grid min="380px">
        <gr-metric-chart title="CPU usage (%)" type="line" [series]="cpuSeries()" [categories]="times()" [colors]="['#17a2b8']"></gr-metric-chart>
        <gr-metric-chart title="RAM free (MB)" type="area" [series]="ramSeries()" [categories]="times()" [colors]="['#28a745']"></gr-metric-chart>
        <gr-metric-chart title="Network speed (Mbps)" type="line" [series]="netSeries()" [categories]="times()" [colors]="['#6f42c1']"></gr-metric-chart>
        <gr-metric-chart title="Disk speed (Mbps)" type="line" [series]="diskSeries()" [categories]="times()" [colors]="['#fd7e14']"></gr-metric-chart>
        <gr-metric-chart title="Assigned jobs" type="area" [series]="loadSeries()" [categories]="times()" [colors]="['#e83e8c']"></gr-metric-chart>
      </gr-card-grid>

      <h2>Connection history</h2>
      <gr-table>
        <thead><tr><th>Connected</th><th>Disconnected</th></tr></thead>
        <tbody>
          @for (c of connections(); track $index) {
            <tr><td>{{ c.connected_at | date: 'short' }}</td><td>{{ c.disconnected_at ? (c.disconnected_at | date: 'short') : 'connected' }}</td></tr>
          } @empty {
            <tr><td colspan="2" class="muted">No sessions recorded.</td></tr>
          }
        </tbody>
      </gr-table>
    </gr-page-layout>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './worker-metrics.component.scss',
})
export class WorkerMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workers = inject(WorkersService);
  private projects = inject(ProjectsService);

  project = '';
  workerId = '';
  projectDisplayName = signal('');
  displayName = signal<string | null>(null);
  samples = signal<WorkerSamplePoint[]>([]);
  connections = signal<WorkerConnectionEntry[]>([]);
  jobsDispatched = signal(0);

  /// The id is the last resort: history outlives the registration that named it.
  workerName = computed(() => this.displayName() || this.workerId);

  times = computed(() => this.samples().map((s) => s.at.slice(11, 16)));
  cpuSeries = computed(() => [{ name: 'cpu', data: this.samples().map((s) => s.cpu_usage_pct ?? 0) }]);
  ramSeries = computed(() => [{ name: 'ram free', data: this.samples().map((s) => s.ram_free_mb ?? 0) }]);
  netSeries = computed(() => [{ name: 'network', data: this.samples().map((s) => s.network_speed_mbps ?? 0) }]);
  diskSeries = computed(() => [{ name: 'disk', data: this.samples().map((s) => s.disk_speed_mbps ?? 0) }]);
  loadSeries = computed(() => [{ name: 'assigned', data: this.samples().map((s) => s.assigned_jobs) }]);

  ngOnInit(): void {
    this.project = this.route.snapshot.paramMap.get('project') ?? '';
    this.workerId = this.route.snapshot.paramMap.get('workerId') ?? '';
    this.projects.getProject(this.project).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.workers.getWorkerMetrics(this.project, this.workerId).subscribe((stats) => {
      this.displayName.set(stats.display_name);
      this.samples.set(stats.samples);
      this.connections.set(stats.connections);
      this.jobsDispatched.set(stats.jobs_dispatched);
    });
  }
}
