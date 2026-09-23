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
  BoardUpstream,
  BoardUpstreamStats,
} from '@core/services/board.service';
import { LiveService } from '@core/services/live.service';
import { LoadingSpinnerComponent, MetricChartComponent } from '@shared/ui';
import { formatBytes, formatCount, formatDuration, formatPercent } from '@shared/text';
import { firstLoad } from '../first-load';

@Component({
  selector: 'app-board-cache',
  standalone: true,
  imports: [CommonModule, MetricChartComponent, LoadingSpinnerComponent],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading cache stats..." />
    } @else {
      <div class="kpis">
        <div class="kpi"><span class="label">Compressed size</span><span class="value">{{ bytes(stats()?.totals?.bytes ?? 0) }}</span></div>
        <div class="kpi"><span class="label">NAR size</span><span class="value">{{ bytes(stats()?.totals?.nar_bytes ?? 0) }}</span></div>
        <div class="kpi"><span class="label">Packages</span><span class="value">{{ count(stats()?.totals?.packages ?? 0) }}</span></div>
        <div class="kpi"><span class="label">Served total</span><span class="value">{{ bytes(stats()?.totals?.bytes_sent_total ?? 0) }}</span></div>
        <div class="kpi"><span class="label">Requests total</span><span class="value">{{ count(stats()?.totals?.requests_total ?? 0) }}</span></div>
      </div>

      <gr-metric-chart
        title="Cache traffic (served per hour)"
        type="area"
        [series]="trafficSeries()"
        [categories]="trafficCats()"
        [colors]="['#17a2b8']"
        [valueFormatter]="bytes"
      ></gr-metric-chart>

      <gr-metric-chart
        title="NAR requests per hour"
        type="line"
        [series]="requestSeries()"
        [categories]="trafficCats()"
        [colors]="['#6f42c1']"
        [valueFormatter]="count"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Storage growth (added per hour)"
        type="area"
        [series]="storageSeries()"
        [categories]="storageCats()"
        [colors]="['#28a745']"
        [valueFormatter]="bytes"
      ></gr-metric-chart>

      <h3 class="upstreams-title">Upstreams</h3>
      @for (u of upstreams()?.upstreams ?? []; track u.upstream_id) {
        <gr-metric-chart
          [title]="upstreamTitle(u)"
          type="line"
          [series]="[{ name: 'latency', data: u.latency.map((p) => p.sum) }]"
          [categories]="u.latency.map((p) => p.bucket_start.slice(11, 16))"
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
  upstreams = signal<BoardUpstreamStats | null>(null);

  trafficCats = computed(() => (this.stats()?.traffic ?? []).map((p) => p.bucket_start.slice(11, 16)));
  trafficSeries = computed(() => [
    { name: 'served', data: (this.stats()?.traffic ?? []).map((p) => p.sum) },
  ]);
  requestSeries = computed(() => [
    { name: 'requests', data: (this.stats()?.traffic ?? []).map((p) => p.count) },
  ]);
  storageCats = computed(() => (this.stats()?.storage ?? []).map((p) => p.bucket_start.slice(11, 16)));
  storageSeries = computed(() => [
    { name: 'added', data: (this.stats()?.storage ?? []).map((p) => p.sum) },
  ]);

  readonly bytes = formatBytes;
  readonly count = formatCount;
  readonly duration = formatDuration;

  upstreamTitle(u: BoardUpstream): string {
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
    this.board.getUpstreams(24).pipe(this.first.track()).subscribe((u) => this.upstreams.set(u));
  }
}
