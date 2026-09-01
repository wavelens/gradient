/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { WorkersService, WorkerSamplePoint, WorkerConnectionEntry } from '@core/services/workers.service';
import { MetricChartComponent } from '@shared/ui';

@Component({
  selector: 'app-worker-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, MetricChartComponent],
  template: `
    <div class="wm">
      <a [routerLink]="['/project', project, 'workers']" class="back">← Workers</a>
      <h1>Worker statistics <span class="mono">{{ workerId }}</span></h1>

      <div class="kpis">
        <div class="kpi"><span class="label">Samples</span><span class="value">{{ samples().length }}</span></div>
        <div class="kpi"><span class="label">Jobs dispatched</span><span class="value">{{ jobsDispatched() }}</span></div>
        <div class="kpi"><span class="label">Sessions</span><span class="value">{{ connections().length }}</span></div>
      </div>

      <div class="charts">
        <gr-metric-chart title="CPU usage (%)" type="line" [series]="cpuSeries()" [categories]="times()" [colors]="['#17a2b8']"></gr-metric-chart>
        <gr-metric-chart title="RAM free (MB)" type="area" [series]="ramSeries()" [categories]="times()" [colors]="['#28a745']"></gr-metric-chart>
        <gr-metric-chart title="Network speed (Mbps)" type="line" [series]="netSeries()" [categories]="times()" [colors]="['#6f42c1']"></gr-metric-chart>
        <gr-metric-chart title="Disk speed (Mbps)" type="line" [series]="diskSeries()" [categories]="times()" [colors]="['#fd7e14']"></gr-metric-chart>
        <gr-metric-chart title="Assigned jobs" type="area" [series]="loadSeries()" [categories]="times()" [colors]="['#e83e8c']"></gr-metric-chart>
      </div>

      <h2>Connection history</h2>
      <table class="sessions">
        <thead><tr><th>Connected</th><th>Disconnected</th></tr></thead>
        <tbody>
          @for (c of connections(); track $index) {
            <tr><td>{{ c.connected_at | date: 'short' }}</td><td>{{ c.disconnected_at ? (c.disconnected_at | date: 'short') : 'connected' }}</td></tr>
          } @empty {
            <tr><td colspan="2" class="muted">No sessions recorded.</td></tr>
          }
        </tbody>
      </table>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './worker-metrics.component.scss',
})
export class WorkerMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workers = inject(WorkersService);

  project = '';
  workerId = '';
  samples = signal<WorkerSamplePoint[]>([]);
  connections = signal<WorkerConnectionEntry[]>([]);
  jobsDispatched = signal(0);

  times = computed(() => this.samples().map((s) => s.at.slice(11, 16)));
  cpuSeries = computed(() => [{ name: 'cpu', data: this.samples().map((s) => s.cpu_usage_pct ?? 0) }]);
  ramSeries = computed(() => [{ name: 'ram free', data: this.samples().map((s) => s.ram_free_mb ?? 0) }]);
  netSeries = computed(() => [{ name: 'network', data: this.samples().map((s) => s.network_speed_mbps ?? 0) }]);
  diskSeries = computed(() => [{ name: 'disk', data: this.samples().map((s) => s.disk_speed_mbps ?? 0) }]);
  loadSeries = computed(() => [{ name: 'assigned', data: this.samples().map((s) => s.assigned_jobs) }]);

  ngOnInit(): void {
    this.project = this.route.snapshot.paramMap.get('project') ?? '';
    this.workerId = this.route.snapshot.paramMap.get('workerId') ?? '';
    this.workers.getWorkerMetrics(this.project, this.workerId).subscribe((stats) => {
      this.samples.set(stats.samples);
      this.connections.set(stats.connections);
      this.jobsDispatched.set(stats.jobs_dispatched);
    });
  }
}
