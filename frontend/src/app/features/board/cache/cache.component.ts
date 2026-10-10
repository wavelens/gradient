/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Subscription } from 'rxjs';
import { auditTime } from 'rxjs/operators';
import {
  BoardService,
  BoardCacheStats,
  BoardUpstreamCache,
  BoardUpstreamCacheStats,
} from '@core/services/board.service';
import { LiveService } from '@core/services/live.service';
import { CardGridComponent, LoadingSpinnerComponent, StatCardComponent } from '@gradient/ui/ui';
import { LabelHelpComponent, MetricChartComponent } from '@shared/ui';
import { clockTime, formatBytes, formatCount, formatDuration, formatPercent } from '@shared/text';
import { firstLoad } from '../first-load';

@Component({
  selector: 'app-board-cache',
  standalone: true,
  imports: [CommonModule, CardGridComponent, StatCardComponent, MetricChartComponent, LabelHelpComponent, LoadingSpinnerComponent],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading cache stats..." />
    } @else {
      <gr-card-grid class="kpis" min="160px">
        <gr-stat-card compact label="Compressed size" [value]="bytes(stats()?.totals?.bytes ?? 0)" />
        <gr-stat-card compact label="NAR size" [value]="bytes(stats()?.totals?.nar_bytes ?? 0)" />
        <gr-stat-card compact label="Packages" [value]="count(stats()?.totals?.packages ?? 0)" />
        <gr-stat-card compact label="Served total" [value]="bytes(stats()?.totals?.bytes_sent_total ?? 0)" />
        <gr-stat-card compact label="Requests total" [value]="count(stats()?.totals?.requests_total ?? 0)" />
      </gr-card-grid>

      <gr-metric-chart
        title="Cache traffic (served per hour)"
        doc="ui/job-board/"
        type="area"
        [series]="trafficSeries()"
        [categories]="trafficCats()"
        [colors]="['#17a2b8']"
        [valueFormatter]="bytes"
      ></gr-metric-chart>

      <gr-metric-chart
        title="NAR requests per hour"
        doc="ui/job-board/"
        type="line"
        [series]="requestSeries()"
        [categories]="trafficCats()"
        [colors]="['#6f42c1']"
        [valueFormatter]="count"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Storage growth (added per hour)"
        doc="ui/job-board/"
        type="area"
        [series]="storageSeries()"
        [categories]="storageCats()"
        [colors]="['#28a745']"
        [valueFormatter]="bytes"
      ></gr-metric-chart>

      <h3 class="upstream-caches-title">Upstream Caches <gr-label-help doc="concepts/caches/#upstream-types" title="About upstream caches" /></h3>
      @for (u of upstreamCacheStats()?.upstream_caches ?? []; track u.upstream_id) {
        <gr-metric-chart
          [title]="upstreamTitle(u)"
          type="line"
          [series]="[{ name: 'latency', data: u.latency.map((p) => p.sum) }]"
          [categories]="u.latency.map((p) => clockTime(p.bucket_start))"
          [colors]="['#fd7e14']"
          [valueFormatter]="duration"
        ></gr-metric-chart>
      }
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache.component.scss',
})
export class BoardCacheComponent implements OnInit, OnDestroy {
  private board = inject(BoardService);
  private live = inject(LiveService);
  private liveSub?: Subscription;
  protected first = firstLoad();
  stats = signal<BoardCacheStats | null>(null);
  upstreamCacheStats = signal<BoardUpstreamCacheStats | null>(null);

  trafficCats = computed(() => (this.stats()?.traffic ?? []).map((p) => clockTime(p.bucket_start)));
  trafficSeries = computed(() => [
    { name: 'served', data: (this.stats()?.traffic ?? []).map((p) => p.sum) },
  ]);
  requestSeries = computed(() => [
    { name: 'requests', data: (this.stats()?.traffic ?? []).map((p) => p.count) },
  ]);
  storageCats = computed(() => (this.stats()?.storage ?? []).map((p) => clockTime(p.bucket_start)));
  storageSeries = computed(() => [
    { name: 'added', data: (this.stats()?.storage ?? []).map((p) => p.sum) },
  ]);

  readonly bytes = formatBytes;
  readonly clockTime = clockTime;
  readonly count = formatCount;
  readonly duration = formatDuration;

  upstreamTitle(u: BoardUpstreamCache): string {
    const lat = u.avg_latency_ms !== null ? formatDuration(u.avg_latency_ms) : 'n/a';
    const hit = u.hit_rate !== null ? `${formatPercent(u.hit_rate)} hit` : 'n/a';
    return `${u.display_name} latency · ${lat} · ${hit} · ${formatCount(u.requests_total)} req`;
  }

  ngOnInit(): void {
    this.load();
    this.liveSub = this.live
      .connect('/board/cache/live')
      .pipe(auditTime(2000))
      .subscribe(() => this.load());
  }

  ngOnDestroy(): void {
    this.liveSub?.unsubscribe();
  }

  private load(): void {
    this.board.getCache(24).pipe(this.first.track()).subscribe((s) => this.stats.set(s));
    this.board.getUpstreamCacheStats(24).pipe(this.first.track()).subscribe((u) => this.upstreamCacheStats.set(u));
  }
}
