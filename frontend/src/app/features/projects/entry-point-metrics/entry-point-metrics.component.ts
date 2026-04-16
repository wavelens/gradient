/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import {
  NgApexchartsModule,
  ApexChart,
  ApexStroke,
  ApexFill,
  ApexXAxis,
  ApexYAxis,
  ApexTooltip,
  ApexGrid,
  ApexDataLabels,
  ApexMarkers,
} from 'ng-apexcharts';
import { ProjectsService, EntryPointMetricPoint, EntryPointMetricsResponse } from '@core/services/projects.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

const CHART_COLORS = {
  buildTime: '#17a2b8',
  outputSize: '#28a745',
  closureSize: '#fd7e14',
  deps: '#e83e8c',
  background: '#21262d',
  border: '#2d333b',
  text: '#abb0b4',
  grid: '#2d333b',
};

type ChartOptions = {
  chart: ApexChart;
  theme: { mode: 'dark' | 'light' };
  stroke: ApexStroke;
  fill: ApexFill;
  colors: string[];
  series: { name: string; data: (number | null)[] }[];
  xaxis: ApexXAxis;
  yaxis: ApexYAxis;
  grid: ApexGrid;
  tooltip: ApexTooltip;
  dataLabels: ApexDataLabels;
  markers: ApexMarkers;
};

@Component({
  selector: 'app-entry-point-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, NgApexchartsModule, LoadingSpinnerComponent],
  templateUrl: './entry-point-metrics.component.html',
  styleUrl: './entry-point-metrics.component.scss',
})
export class EntryPointMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);
  private orgsService = inject(OrganizationsService);

  loading = signal(true);
  points = signal<EntryPointMetricPoint[]>([]);
  evalAttr = signal('');
  keepEvaluations = signal(30);
  orgName = '';
  orgDisplayName = signal('');
  projectName = '';
  projectDisplayName = signal('');

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    const evalParam = this.route.snapshot.queryParamMap.get('eval') || '';
    this.evalAttr.set(evalParam);
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.projectsService.getProjectInfo(this.orgName, this.projectName).subscribe({
      next: (proj) => this.projectDisplayName.set(proj.display_name),
      error: () => {},
    });

    this.projectsService.getEntryPointMetrics(this.orgName, this.projectName, evalParam).subscribe({
      next: (data: EntryPointMetricsResponse) => {
        // Substituted builds didn't actually run on a server in this evaluation,
        // so their build time / sizes are not meaningful trend data points.
        this.points.set(data.points.filter((p) => p.build_status !== 'Substituted'));
        this.keepEvaluations.set(data.keep_evaluations);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  private get labels(): string[] {
    return this.points().map((p) => this.formatDate(p.created_at));
  }

  private baseChart(color: string): ChartOptions {
    return {
      chart: {
        type: 'area',
        height: 220,
        background: CHART_COLORS.background,
        toolbar: { show: false },
        animations: { enabled: true, speed: 400 },
        zoom: { enabled: false },
      },
      theme: { mode: 'dark' },
      stroke: { curve: 'smooth', width: 2 },
      fill: {
        type: 'gradient',
        gradient: { shadeIntensity: 0.3, opacityFrom: 0.4, opacityTo: 0.02, stops: [0, 100] },
      },
      colors: [color],
      series: [],
      xaxis: {
        categories: this.labels,
        labels: { style: { colors: CHART_COLORS.text, fontSize: '11px' }, rotate: -30 },
        axisBorder: { color: CHART_COLORS.border },
        axisTicks: { color: CHART_COLORS.border },
      },
      yaxis: { labels: { style: { colors: CHART_COLORS.text } } },
      grid: { borderColor: CHART_COLORS.grid, strokeDashArray: 3 },
      tooltip: { theme: 'dark' },
      dataLabels: { enabled: false },
      markers: { size: 3, strokeWidth: 0 },
    };
  }

  buildTimeChart = computed<ChartOptions>(() => {
    const pts = this.points();
    const opts = this.baseChart(CHART_COLORS.buildTime);
    opts.series = [{ name: 'Build time', data: pts.map((p) => p.build_time_ms !== null ? Math.round(p.build_time_ms / 1000) : null) }];
    opts.yaxis = { ...opts.yaxis, title: { text: 'seconds', style: { color: CHART_COLORS.text } }, labels: { style: { colors: CHART_COLORS.text }, formatter: (v: number) => `${v}s` } };
    opts.tooltip = { theme: 'dark', y: { formatter: (v: number) => this.formatDuration(v * 1000) } };
    return opts;
  });

  outputSizeChart = computed<ChartOptions>(() => {
    const pts = this.points();
    const opts = this.baseChart(CHART_COLORS.outputSize);
    opts.series = [{ name: 'Output size', data: pts.map((p) => p.output_size_bytes) }];
    opts.yaxis = { ...opts.yaxis, labels: { style: { colors: CHART_COLORS.text }, formatter: (v: number) => this.formatBytes(v) } };
    opts.tooltip = { theme: 'dark', y: { formatter: (v: number) => this.formatBytes(v) } };
    return opts;
  });

  closureSizeChart = computed<ChartOptions>(() => {
    const pts = this.points();
    const opts = this.baseChart(CHART_COLORS.closureSize);
    opts.series = [{ name: 'Closure size', data: pts.map((p) => p.closure_size_bytes) }];
    opts.yaxis = { ...opts.yaxis, labels: { style: { colors: CHART_COLORS.text }, formatter: (v: number) => this.formatBytes(v) } };
    opts.tooltip = { theme: 'dark', y: { formatter: (v: number) => this.formatBytes(v) } };
    return opts;
  });

  depsChart = computed<ChartOptions>(() => {
    const pts = this.points();
    const opts = this.baseChart(CHART_COLORS.deps);
    opts.series = [{ name: 'Dependencies', data: pts.map((p) => p.dependencies_count) }];
    opts.yaxis = { ...opts.yaxis, labels: { style: { colors: CHART_COLORS.text }, formatter: (v: number) => String(Math.round(v)) } };
    opts.tooltip = { theme: 'dark', y: { formatter: (v: number) => `${Math.round(v)} deps` } };
    return opts;
  });

  completedCount = computed(() => this.points().filter((p) => p.build_status === 'Completed').length);
  failedCount = computed(() => this.points().filter((p) => p.build_status === 'Failed').length);

  formatBytes(bytes: number): string {
    if (!bytes || bytes === 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[Math.min(i, units.length - 1)]}`;
  }

  formatDuration(ms: number): string {
    const s = Math.round(ms / 1000);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    if (h > 0) return `${h}h ${m}m ${sec}s`;
    if (m > 0) return `${m}m ${sec}s`;
    return `${sec}s`;
  }

  private formatDate(iso: string): string {
    const d = new Date(iso.includes('Z') || iso.includes('+') ? iso : iso + 'Z');
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' }) + ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
}
