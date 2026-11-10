/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { MetricChartComponent } from '@shared/ui';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { formatDependencyCount, metricSeriesChartColorTokenNames } from '../common-chart-style';
import { formatBytes, formatDuration, monthDayClockTime } from '@shared/text';
import { TaskMetricPoint, TaskMetricsResponse } from '@core/models';

@Component({
  selector: 'app-task-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, MetricChartComponent, LoadStateComponent, PageLayoutComponent],
  templateUrl: './task-metrics.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-metrics.component.scss',
})
export class TaskMetricsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private tasksService = inject(TasksService);
  private crumbs = inject(BreadcrumbsService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  metrics = signal<TaskMetricPoint[]>([]);
  keepEvaluations = signal(30);
  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.task(this.projectName, this.taskName, { label: 'Metrics' }));

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.tasksService.getTaskMetrics(this.projectName, this.taskName).subscribe({
      next: (data: TaskMetricsResponse) => {
        this.metrics.set(data.points);
        this.keepEvaluations.set(data.keep_evaluations);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  labels = computed(() => this.metrics().map((p) => monthDayClockTime(p.created_at)));

  buildTimeSeries = computed(() => [
    { name: 'Build time', data: this.metrics().map((p) => p.build_time_total_ms) },
  ]);
  evalTimeSeries = computed(() => [
    { name: 'Eval time', data: this.metrics().map((p) => p.eval_time_ms ?? null) },
  ]);
  outputSizeSeries = computed(() => [
    { name: 'Output size', data: this.metrics().map((p) => p.output_size_bytes ?? null) },
  ]);
  closureSizeSeries = computed(() => [
    { name: 'Build closure', data: this.metrics().map((p) => p.closure_size_bytes ?? null) },
    { name: 'Runtime closure', data: this.metrics().map((p) => p.runtime_closure_size_bytes ?? null), isLineDashed: true },
  ]);
  depsSeries = computed(() => [
    { name: 'Dependencies', data: this.metrics().map((p) => p.dependencies_count) },
  ]);

  readonly buildTimeColor = [metricSeriesChartColorTokenNames.buildTime];
  readonly evalTimeColor = [metricSeriesChartColorTokenNames.evalTime];
  readonly outputSizeColor = [metricSeriesChartColorTokenNames.outputSize];
  readonly closureSizeColors = [metricSeriesChartColorTokenNames.closureSize, metricSeriesChartColorTokenNames.runtimeClosure];
  readonly depsColor = [metricSeriesChartColorTokenNames.deps];

  readonly formatDuration = formatDuration;
  readonly formatBytes = formatBytes;
  readonly formatDeps = formatDependencyCount;
}
