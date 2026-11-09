/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import {
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { MetricChartComponent } from '@shared/ui';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { formatBytes, formatCount, formatDuration, monthDayClockTime } from '@shared/text';
import { buildPhase, buildStatusLabel } from '@shared/evaluation';
import { BuildStatus, EntryPointMetricPoint, EntryPointMetricsResponse } from '@core/models';
import { links } from '@shared/routing';
import { getEntryPointShortName } from '../task-detail/entry-point-groups';

const CHART_COLORS = {
  buildTime: '--gr-chart-cyan',
  outputSize: '--gr-chart-green',
  closureSize: '--gr-chart-orange',
  runtimeClosure: '--gr-chart-teal',
  deps: '--gr-chart-pink',
};

@Component({
  selector: 'app-entry-point-metrics',
  standalone: true,
  imports: [CommonModule, RouterModule, ButtonComponent, MetricChartComponent, LoadStateComponent, PageLayoutComponent],
  templateUrl: './entry-point-metrics.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './entry-point-metrics.component.scss',
})
export class EntryPointMetricsComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private tasksService = inject(TasksService);
  private crumbs = inject(BreadcrumbsService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  points = signal<EntryPointMetricPoint[]>([]);
  evalAttr = signal('');
  shortEntryPointName = computed(() => getEntryPointShortName(this.evalAttr()));
  keepEvaluations = signal(30);
  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.task(
    this.projectName,
    this.taskName,
    { label: 'Metrics', link: links.taskPage(this.projectName, this.taskName, 'metrics') },
    { label: 'Entry Point Metrics' },
  ));

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    const evalParam = this.route.snapshot.queryParamMap.get('eval') || '';
    this.evalAttr.set(evalParam);
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.tasksService.getEntryPointMetrics(this.projectName, this.taskName, this.evalAttr()).subscribe({
      next: (data: EntryPointMetricsResponse) => {
        this.points.set(data.points);
        this.keepEvaluations.set(data.keep_evaluations);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  labels = computed(() => this.points().map((p) => monthDayClockTime(p.created_at)));

  buildTimeSeries = computed(() => [
    {
      name: 'Build time',
      data: this.points().map((p) => p.build_time_ms ?? null),
    },
  ]);
  outputSizeSeries = computed(() => [
    { name: 'Output size', data: this.points().map((p) => p.output_size_bytes ?? null) },
  ]);
  closureSizeSeries = computed(() => [
    { name: 'Build closure', data: this.points().map((p) => p.closure_size_bytes ?? null) },
    { name: 'Runtime closure', data: this.points().map((p) => p.runtime_closure_size_bytes ?? null) },
  ]);
  depsSeries = computed(() => [
    { name: 'Dependencies', data: this.points().map((p) => p.dependencies_count) },
  ]);

  readonly buildTimeColor = [CHART_COLORS.buildTime];
  readonly outputSizeColor = [CHART_COLORS.outputSize];
  readonly closureSizeColors = [CHART_COLORS.closureSize, CHART_COLORS.runtimeClosure];
  readonly depsColor = [CHART_COLORS.deps];

  readonly formatDuration = formatDuration;
  readonly formatBytes = formatBytes;
  readonly formatDeps = (v: number) => `${formatCount(v)} deps`;

  latestBuildId = computed(() => {
    const pts = this.points();
    return pts.length ? pts[pts.length - 1].build_id : '';
  });

  buildStatusCounts = computed(() => {
    const counts = new Map<BuildStatus, number>();
    for (const p of this.points()) counts.set(p.build_status, (counts.get(p.build_status) ?? 0) + 1);
    return [...counts]
      .sort(([, a], [, b]) => b - a)
      .map(([status, count]) => ({ status, count, label: buildStatusLabel(status), phase: buildPhase(status) }));
  });
}
