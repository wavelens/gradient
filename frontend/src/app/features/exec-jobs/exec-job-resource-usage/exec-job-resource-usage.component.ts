/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DOCUMENT, computed, inject, input, signal } from '@angular/core';
import { takeUntilDestroyed, toObservable } from '@angular/core/rxjs-interop';
import { EMPTY, Subject, catchError, combineLatest, of, startWith, switchMap, takeWhile, throwError } from 'rxjs';
import { LoadStateComponent, getForbiddenOrMissingKindFromError } from '@gradient/ui/ui';
import { MetricChartComponent, type MetricSeries } from '@shared/ui';
import { refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible } from '@shared/paging';
import { formatBytes, formatPercent } from '@shared/text';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import type { ExecJobMetricsResponse } from '@core/models';
import { metricSeriesChartColorTokenNames } from '@features/tasks/common-chart-style';
import { cpusLabel } from '../exec-job-status';

const metricsPollIntervalMilliseconds = 2000;
const sharedChartMargins = { left: 72, right: 24 };

function formatElapsedMillisecondsAsHoursMinutesSeconds(milliseconds: number): string {
  const seconds = Math.floor(milliseconds / 1000);
  const minutes = String(Math.floor(seconds / 60) % 60).padStart(2, '0');
  return `${Math.floor(seconds / 3600)}:${minutes}:${String(seconds % 60).padStart(2, '0')}`;
}

function formatCpuCountToOneDecimal(cpus: number): string {
  return String(Math.round(cpus * 10) / 10);
}

interface ResourcePeakTile {
  resourcePeakTitle: string;
  peakAgainstSizeText: string;
  peakPercentOfUsableCapacityText: string;
  averageUsageText: string | null;
}

@Component({
  selector: 'gr-exec-job-resource-usage',
  standalone: true,
  imports: [LoadStateComponent, MetricChartComponent],
  templateUrl: './exec-job-resource-usage.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './exec-job-resource-usage.component.scss',
})
export class ExecJobResourceUsageComponent {
  private execJobs = inject(ExecJobsService);
  private document = inject(DOCUMENT);
  private retryLoadRequest = new Subject<void>();

  job = input.required<string>();
  attempt = input<number | null>(null);
  node = input<number | null>(null);

  isAwaitingFirstResourceUsageResponse = signal(true);
  loadError = signal<unknown>(null);
  execJobMetricsResponse = signal<ExecJobMetricsResponse | null>(null);

  private resourceUsageRequestKey = computed(() => ({ job: this.job(), attempt: this.attempt(), node: this.node() }));

  readonly sharedChartMargins = sharedChartMargins;
  readonly formatBytes = formatBytes;
  readonly formatCpuCountToOneDecimal = formatCpuCountToOneDecimal;
  readonly averagePeakUsableSizeSeriesColors = [
    metricSeriesChartColorTokenNames.averageUsageLine,
    metricSeriesChartColorTokenNames.peakUsageLineColor,
    metricSeriesChartColorTokenNames.guestUsableSizeLineColor,
  ];
  readonly diskChartPeakAndUsableSizeColors = [
    metricSeriesChartColorTokenNames.peakUsageLineColor,
    metricSeriesChartColorTokenNames.guestUsableSizeLineColor,
  ];

  resourceUsageSummaryTiles = computed<ResourcePeakTile[]>(() => {
    const metrics = this.execJobMetricsResponse();
    if (!metrics?.points.length) return [];
    const ramAverageBytes = metrics.points.reduce((sum, point) => sum + point.ram_avg_bytes, 0) / metrics.points.length;
    return [
      {
        resourcePeakTitle: 'CPU peak',
        peakAgainstSizeText: `${formatCpuCountToOneDecimal(metrics.cpu_max)} / ${cpusLabel(metrics.requested_cpus)}`,
        peakPercentOfUsableCapacityText: `${formatPercent(metrics.cpu_max / metrics.requested_cpus)} of usable capacity`,
        averageUsageText: `Average ${cpusLabel(Math.round(metrics.cpu_avg * 10) / 10)}`,
      },
      {
        resourcePeakTitle: 'RAM peak',
        peakAgainstSizeText: `${formatBytes(metrics.ram_max_bytes)} / ${formatBytes(metrics.usable_memory_bytes)} usable (${formatBytes(metrics.requested_memory_bytes)} requested)`,
        peakPercentOfUsableCapacityText: `${formatPercent(metrics.ram_max_bytes / (metrics.usable_memory_bytes ?? NaN))} of usable capacity`,
        averageUsageText: `Average ${formatBytes(ramAverageBytes)}`,
      },
      {
        resourcePeakTitle: 'Disk peak',
        peakAgainstSizeText: `${formatBytes(metrics.disk_max_bytes)} / ${formatBytes(metrics.usable_disk_bytes)} usable (${formatBytes(metrics.requested_disk_bytes)} requested)`,
        peakPercentOfUsableCapacityText: `${formatPercent(metrics.disk_max_bytes / (metrics.usable_disk_bytes ?? NaN))} of usable capacity`,
        averageUsageText: null,
      },
    ];
  });

  elapsedTimeAxisLabels = computed(() =>
    (this.execJobMetricsResponse()?.points ?? []).map((point) => formatElapsedMillisecondsAsHoursMinutesSeconds(point.elapsed_ms)));

  cpuInUseChartSeries = computed<MetricSeries[]>(() => {
    const metrics = this.execJobMetricsResponse();
    const points = metrics?.points ?? [];
    return [
      { name: 'Average', data: points.map((point) => point.cpu_avg) },
      { name: 'Peak', data: points.map((point) => point.cpu_max) },
      { name: 'Usable', data: points.map(() => metrics?.requested_cpus ?? null), isLineDashed: true },
    ];
  });

  ramUsedBytesAveragePeakAndUsableMemoryChartSeries = computed<MetricSeries[]>(() => {
    const metrics = this.execJobMetricsResponse();
    const points = metrics?.points ?? [];
    return [
      { name: 'Average', data: points.map((point) => point.ram_avg_bytes) },
      { name: 'Peak', data: points.map((point) => point.ram_max_bytes) },
      { name: 'Usable', data: points.map(() => metrics?.usable_memory_bytes ?? null), isLineDashed: true },
    ];
  });

  workDiskUsedBytesChartSeries = computed<MetricSeries[]>(() => {
    const metrics = this.execJobMetricsResponse();
    const points = metrics?.points ?? [];
    return [
      { name: 'Peak', data: points.map((point) => point.disk_max_bytes) },
      { name: 'Usable', data: points.map(() => metrics?.usable_disk_bytes ?? null), isLineDashed: true },
    ];
  });

  constructor() {
    combineLatest([toObservable(this.resourceUsageRequestKey), this.retryLoadRequest.pipe(startWith(undefined))]).pipe(
      switchMap(([{ job, attempt, node }]) => {
        this.isAwaitingFirstResourceUsageResponse.set(true);
        this.execJobMetricsResponse.set(null);
        this.loadError.set(null);
        return refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible(this.document, metricsPollIntervalMilliseconds).pipe(
          switchMap(() => this.execJobs.getJobAttemptMetrics(job, attempt, node).pipe(
            catchError((error: unknown) => (getForbiddenOrMissingKindFromError(error) === 'missing' ? of(null) : throwError(() => error))),
          )),
          takeWhile((metrics) => metrics?.attempt_open === true, true),
          catchError((error: unknown) => {
            this.loadError.set(error);
            this.isAwaitingFirstResourceUsageResponse.set(false);
            return EMPTY;
          }),
        );
      }),
      takeUntilDestroyed(),
    ).subscribe((metrics) => {
      this.execJobMetricsResponse.set(metrics);
      this.isAwaitingFirstResourceUsageResponse.set(false);
    });
  }

  retryLoadResourceUsageMetrics(): void {
    this.retryLoadRequest.next();
  }
}
