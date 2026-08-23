/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';
import { TasksService, EntryPointMetricPoint, EntryPointMetricsResponse } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ButtonComponent } from '@shared/ui';

const CHART_COLORS = {
  buildTime: '#17a2b8',
  outputSize: '#28a745',
  closureSize: '#fd7e14',
  runtimeClosure: '#20c997',
  deps: '#e83e8c',
};

@Component({
  selector: 'app-entry-point-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, ButtonComponent, MetricChartComponent, LoadingSpinnerComponent],
  templateUrl: './entry-point-metrics.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './entry-point-metrics.component.scss',
})
export class EntryPointMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private tasksService = inject(TasksService);
  private projectsService = inject(ProjectsService);

  loading = signal(true);
  points = signal<EntryPointMetricPoint[]>([]);
  evalAttr = signal('');
  keepEvaluations = signal(30);
  projectName = '';
  projectDisplayName = signal('');
  taskName = '';
  taskDisplayName = signal('');

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    const evalParam = this.route.snapshot.queryParamMap.get('eval') || '';
    this.evalAttr.set(evalParam);
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.tasksService.getTaskInfo(this.projectName, this.taskName).subscribe({
      next: (proj) => this.taskDisplayName.set(proj.display_name),
      error: () => {},
    });

    this.tasksService.getEntryPointMetrics(this.projectName, this.taskName, evalParam).subscribe({
      next: (data: EntryPointMetricsResponse) => {
        this.points.set(data.points);
        this.keepEvaluations.set(data.keep_evaluations);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  labels = computed(() => this.points().map((p) => this.formatDate(p.created_at)));

  buildTimeSeries = computed(() => [
    {
      name: 'Build time',
      data: this.points().map((p) => (p.build_time_ms !== null ? Math.round(p.build_time_ms / 1000) : null)),
    },
  ]);
  outputSizeSeries = computed(() => [
    { name: 'Output size', data: this.points().map((p) => p.output_size_bytes) },
  ]);
  closureSizeSeries = computed(() => [
    { name: 'Build closure', data: this.points().map((p) => p.closure_size_bytes) },
    { name: 'Runtime closure', data: this.points().map((p) => p.runtime_closure_size_bytes) },
  ]);
  depsSeries = computed(() => [
    { name: 'Dependencies', data: this.points().map((p) => p.dependencies_count) },
  ]);

  readonly buildTimeColor = [CHART_COLORS.buildTime];
  readonly outputSizeColor = [CHART_COLORS.outputSize];
  readonly closureSizeColors = [CHART_COLORS.closureSize, CHART_COLORS.runtimeClosure];
  readonly depsColor = [CHART_COLORS.deps];

  readonly formatSeconds = (v: number) => this.formatDuration(v * 1000);
  readonly formatSize = (v: number) => this.formatBytes(v);
  readonly formatDeps = (v: number) => `${Math.round(v)} deps`;

  latestBuildId = computed(() => {
    const pts = this.points();
    return pts.length ? pts[pts.length - 1].build_id : '';
  });

  completedCount = computed(() => this.points().filter((p) => p.build_status === 'Completed').length);
  failedCount = computed(() => this.points().filter((p) => p.build_status === 'FailedPermanent' || p.build_status === 'FailedTimeout').length);
  substitutedCount = computed(() => this.points().filter((p) => p.build_status === 'Substituted').length);

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
