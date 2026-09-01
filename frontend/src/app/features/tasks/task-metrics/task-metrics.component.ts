/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { IconComponent, LoadingSpinnerComponent, MetricChartComponent } from '@shared/ui';
import { TasksService, TaskMetricPoint, TaskMetricsResponse } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';

const CHART_COLORS = {
  buildTime: '#17a2b8',
  evalTime: '#6f42c1',
  outputSize: '#28a745',
  closureSize: '#fd7e14',
  runtimeClosure: '#20c997',
  deps: '#e83e8c',
};

@Component({
  selector: 'app-task-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, MetricChartComponent, LoadingSpinnerComponent, IconComponent],
  templateUrl: './task-metrics.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-metrics.component.scss',
})
export class TaskMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private tasksService = inject(TasksService);
  private projectsService = inject(ProjectsService);

  loading = signal(true);
  metrics = signal<TaskMetricPoint[]>([]);
  keepEvaluations = signal(30);
  projectName = '';
  projectDisplayName = signal('');
  taskName = '';
  taskDisplayName = signal('');

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.tasksService.getTaskInfo(this.projectName, this.taskName).subscribe({
      next: (proj) => this.taskDisplayName.set(proj.display_name),
      error: () => {},
    });
    this.tasksService.getTaskMetrics(this.projectName, this.taskName).subscribe({
      next: (data: TaskMetricsResponse) => {
        this.metrics.set(data.points);
        this.keepEvaluations.set(data.keep_evaluations);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  labels = computed(() => this.metrics().map((p) => this.formatDate(p.created_at)));

  buildTimeSeries = computed(() => [
    { name: 'Build time', data: this.metrics().map((p) => Math.round(p.build_time_total_ms / 1000)) },
  ]);
  evalTimeSeries = computed(() => [
    { name: 'Eval time', data: this.metrics().map((p) => Math.round(p.eval_time_ms / 1000)) },
  ]);
  outputSizeSeries = computed(() => [
    { name: 'Output size', data: this.metrics().map((p) => p.output_size_bytes) },
  ]);
  closureSizeSeries = computed(() => [
    { name: 'Build closure', data: this.metrics().map((p) => p.closure_size_bytes) },
    { name: 'Runtime closure', data: this.metrics().map((p) => p.runtime_closure_size_bytes) },
  ]);
  depsSeries = computed(() => [
    { name: 'Dependencies', data: this.metrics().map((p) => p.dependencies_count) },
  ]);

  readonly buildTimeColor = [CHART_COLORS.buildTime];
  readonly evalTimeColor = [CHART_COLORS.evalTime];
  readonly outputSizeColor = [CHART_COLORS.outputSize];
  readonly closureSizeColors = [CHART_COLORS.closureSize, CHART_COLORS.runtimeClosure];
  readonly depsColor = [CHART_COLORS.deps];

  readonly formatSeconds = (v: number) => this.formatDuration(v * 1000);
  readonly formatSize = (v: number) => this.formatBytes(v);
  readonly formatDeps = (v: number) => `${Math.round(v)} deps`;

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
