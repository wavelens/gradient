/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { WorkersService, WorkerSamplePoint, WorkerConnectionEntry } from '@core/services/workers.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-worker-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, MetricChartComponent],
  template: `
    <div class="wm">
      <a [routerLink]="['/organization', org, 'workers']" class="back">← Workers</a>
      <h1>Worker statistics <span class="mono">{{ workerId }}</span></h1>

      <div class="kpis">
        <div class="kpi"><span class="label">Samples</span><span class="value">{{ samples().length }}</span></div>
        <div class="kpi"><span class="label">Jobs dispatched</span><span class="value">{{ jobsDispatched() }}</span></div>
        <div class="kpi"><span class="label">Sessions</span><span class="value">{{ connections().length }}</span></div>
      </div>

      <div class="charts">
        <app-metric-chart title="CPU usage (%)" type="line" [series]="cpuSeries()" [categories]="times()" [colors]="['#17a2b8']"></app-metric-chart>
        <app-metric-chart title="RAM free (MB)" type="area" [series]="ramSeries()" [categories]="times()" [colors]="['#28a745']"></app-metric-chart>
        <app-metric-chart title="Network speed (Mbps)" type="line" [series]="netSeries()" [categories]="times()" [colors]="['#6f42c1']"></app-metric-chart>
        <app-metric-chart title="Disk speed (Mbps)" type="line" [series]="diskSeries()" [categories]="times()" [colors]="['#fd7e14']"></app-metric-chart>
        <app-metric-chart title="Assigned jobs" type="area" [series]="loadSeries()" [categories]="times()" [colors]="['#e83e8c']"></app-metric-chart>
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
  styles: [
    `
      .wm { padding: 1.5rem; max-width: 1200px; margin: 0 auto; }
      .back { color: #abb0b4; text-decoration: none; font-size: 0.85rem; }
      h1 { color: #fff; font-size: 1.4rem; margin: 0.5rem 0 1rem; }
      h2 { color: #fff; font-size: 1.1rem; margin: 1.5rem 0 0.75rem; }
      .mono { font-family: monospace; color: #abb0b4; font-size: 1rem; }
      .kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 1rem; margin-bottom: 1.5rem; }
      .kpi { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; display: flex; flex-direction: column; gap: 0.25rem; }
      .kpi .label { color: #abb0b4; font-size: 0.8rem; }
      .kpi .value { color: #fff; font-size: 1.6rem; font-weight: 600; }
      .charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 1rem; }
      table.sessions { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .muted { color: #818181; }
    `,
  ],
})
export class WorkerMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workers = inject(WorkersService);

  org = '';
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
    this.org = this.route.snapshot.paramMap.get('org') ?? '';
    this.workerId = this.route.snapshot.paramMap.get('workerId') ?? '';
    this.workers.getWorkerMetrics(this.org, this.workerId).subscribe((stats) => {
      this.samples.set(stats.samples);
      this.connections.set(stats.connections);
      this.jobsDispatched.set(stats.jobs_dispatched);
    });
  }
}
