/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Subscription } from 'rxjs';
import { auditTime } from 'rxjs/operators';
import { BoardService, BoardCacheStats, BoardUpstreamStats } from '@core/services/board.service';
import { LiveService } from '@core/services/live.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

const GIB = 1024 ** 3;

@Component({
  selector: 'app-board-cache',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <div class="kpis">
      <div class="kpi"><span class="label">Compressed size</span><span class="value">{{ gib(stats()?.totals?.bytes) }} GiB</span></div>
      <div class="kpi"><span class="label">NAR size</span><span class="value">{{ gib(stats()?.totals?.nar_bytes) }} GiB</span></div>
      <div class="kpi"><span class="label">Packages</span><span class="value">{{ stats()?.totals?.packages ?? 0 }}</span></div>
      <div class="kpi"><span class="label">Served total</span><span class="value">{{ gib(stats()?.totals?.bytes_sent_total) }} GiB</span></div>
      <div class="kpi"><span class="label">Requests total</span><span class="value">{{ stats()?.totals?.requests_total ?? 0 }}</span></div>
    </div>

    <app-metric-chart
      title="Cache traffic (GiB served per hour)"
      type="area"
      [series]="trafficSeries()"
      [categories]="trafficCats()"
      [colors]="['#17a2b8']"
    ></app-metric-chart>

    <app-metric-chart
      title="NAR requests per hour"
      type="line"
      [series]="requestSeries()"
      [categories]="trafficCats()"
      [colors]="['#6f42c1']"
    ></app-metric-chart>

    <app-metric-chart
      title="Storage growth (GiB added per hour)"
      type="area"
      [series]="storageSeries()"
      [categories]="storageCats()"
      [colors]="['#28a745']"
    ></app-metric-chart>

    <h3 class="upstreams-title">Upstreams</h3>
    <div class="upstreams">
      @for (u of upstreams()?.upstreams ?? []; track u.upstream_id) {
        <div class="upstream">
          <div class="head">
            <span class="name">{{ u.display_name }}</span>
            <span class="meta">
              {{ u.avg_latency_ms !== null ? (u.avg_latency_ms | number: '1.0-0') + ' ms' : 'n/a' }}
              &middot;
              {{ u.hit_rate !== null ? (u.hit_rate * 100 | number: '1.0-0') + '% hit' : 'n/a' }}
              &middot; {{ u.requests_total }} req
            </span>
          </div>
          <app-metric-chart
            [title]="'Latency (ms) - ' + u.display_name"
            type="line"
            [series]="[{ name: 'latency', data: u.latency.map((p) => p.sum) }]"
            [categories]="u.latency.map((p) => p.bucket_start.slice(11, 16))"
            [colors]="['#fd7e14']"
          ></app-metric-chart>
        </div>
      }
    </div>
  `,
  styles: [
    `
      .kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 1rem; margin-bottom: 1.5rem; }
      .kpi { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; display: flex; flex-direction: column; gap: 0.25rem; }
      .kpi .label { color: #abb0b4; font-size: 0.8rem; }
      .kpi .value { color: #fff; font-size: 1.4rem; font-weight: 600; }
      app-metric-chart { display: block; margin-bottom: 1rem; }
    `,
    `
      .upstreams-title { color: #fff; margin: 1.5rem 0 0.75rem; }
      .upstreams { display: flex; flex-direction: column; gap: 1rem; }
      .upstream { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; }
      .upstream .head { display: flex; justify-content: space-between; margin-bottom: 0.5rem; }
      .upstream .name { color: #fff; font-weight: 600; }
      .upstream .meta { color: #abb0b4; font-size: 0.85rem; }
    `,
  ],
})
export class BoardCacheComponent implements OnInit, OnDestroy {
  private board = inject(BoardService);
  private live = inject(LiveService);
  private liveSub?: Subscription;
  stats = signal<BoardCacheStats | null>(null);
  upstreams = signal<BoardUpstreamStats | null>(null);

  trafficCats = computed(() => (this.stats()?.traffic ?? []).map((p) => p.bucket_start.slice(11, 16)));
  trafficSeries = computed(() => [
    { name: 'served', data: (this.stats()?.traffic ?? []).map((p) => +(p.sum / GIB).toFixed(3)) },
  ]);
  requestSeries = computed(() => [
    { name: 'requests', data: (this.stats()?.traffic ?? []).map((p) => p.count) },
  ]);
  storageCats = computed(() => (this.stats()?.storage ?? []).map((p) => p.bucket_start.slice(11, 16)));
  storageSeries = computed(() => [
    { name: 'added', data: (this.stats()?.storage ?? []).map((p) => +(p.sum / GIB).toFixed(3)) },
  ]);

  gib(bytes: number | undefined): string {
    return ((bytes ?? 0) / GIB).toFixed(2);
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
    this.board.getCache(24).subscribe((s) => this.stats.set(s));
    this.board.getUpstreams(24).subscribe((u) => this.upstreams.set(u));
  }
}
