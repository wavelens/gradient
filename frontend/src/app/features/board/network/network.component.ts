/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, BoardNetworkStats, HttpRouteStat } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

const GIB = 1024 ** 3;

type HttpSortKey = keyof Pick<HttpRouteStat, 'method' | 'route' | 'count' | 'avg_ms' | 'errors'>;

@Component({
  selector: 'app-board-network',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <app-metric-chart
      title="NAR egress (GiB served per hour)"
      type="area"
      [series]="egressSeries()"
      [categories]="egressCats()"
      [colors]="['#17a2b8']"
    ></app-metric-chart>

    <app-metric-chart
      title="Worker network speed (Mbps, latest sample)"
      type="bar"
      [series]="netSeries()"
      [categories]="workerCats()"
      [colors]="['#6f42c1']"
    ></app-metric-chart>

    <app-metric-chart
      title="Worker disk speed (Mbps, latest sample)"
      type="bar"
      [series]="diskSeries()"
      [categories]="workerCats()"
      [colors]="['#fd7e14']"
    ></app-metric-chart>

    <h2>HTTP routes @if (!stats()?.http?.length) {<span class="muted">(superuser-only)</span>}</h2>
    <table class="http">
      <thead>
        <tr>
          <th class="sortable" (click)="sortBy('method')">Method{{ sortIndicator('method') }}</th>
          <th class="sortable" (click)="sortBy('route')">Route{{ sortIndicator('route') }}</th>
          <th class="sortable num" (click)="sortBy('count')">Requests{{ sortIndicator('count') }}</th>
          <th class="sortable num" (click)="sortBy('avg_ms')">Avg ms{{ sortIndicator('avg_ms') }}</th>
          <th class="sortable num" (click)="sortBy('errors')">Errors{{ sortIndicator('errors') }}</th>
        </tr>
      </thead>
      <tbody>
        @for (r of sortedHttp(); track r.method + r.route) {
          <tr>
            <td>{{ r.method }}</td>
            <td class="mono">{{ r.route }}</td>
            <td class="num">{{ r.count }}</td>
            <td class="num">{{ r.avg_ms | number: '1.1-1' }}</td>
            <td class="num" [class.bad]="r.errors > 0">{{ r.errors }}</td>
          </tr>
        } @empty {
          <tr><td colspan="5" class="muted">No HTTP route data.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      app-metric-chart { display: block; margin-bottom: 1rem; }
      h2 { color: #fff; font-size: 1.05rem; margin: 1.5rem 0 0.75rem; }
      table.http { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      th.sortable { cursor: pointer; user-select: none; white-space: nowrap; }
      th.sortable:hover { color: #17a2b8; }
      .mono { font-family: monospace; }
      .num { text-align: right; font-variant-numeric: tabular-nums; }
      .num.bad { color: #dc3545; }
      .muted { color: #818181; font-weight: 400; font-size: 0.85rem; }
    `,
  ],
})
export class BoardNetworkComponent implements OnInit {
  private board = inject(BoardService);
  stats = signal<BoardNetworkStats | null>(null);
  sortKey = signal<HttpSortKey>('count');
  sortAsc = signal(false);

  sortedHttp = computed(() => {
    const rows = [...(this.stats()?.http ?? [])];
    const key = this.sortKey();
    const dir = this.sortAsc() ? 1 : -1;
    return rows.sort((a, b) => {
      const av = a[key];
      const bv = b[key];
      const cmp = typeof av === 'string' ? av.localeCompare(bv as string) : (av as number) - (bv as number);
      return cmp * dir;
    });
  });

  sortBy(key: HttpSortKey): void {
    if (this.sortKey() === key) {
      this.sortAsc.update((v) => !v);
    } else {
      this.sortKey.set(key);
      this.sortAsc.set(true);
    }
  }

  sortIndicator(key: HttpSortKey): string {
    if (this.sortKey() !== key) return '';
    return this.sortAsc() ? ' ▲' : ' ▼';
  }

  egressCats = computed(() => (this.stats()?.nar_egress ?? []).map((p) => p.bucket_start.slice(11, 16)));
  egressSeries = computed(() => [
    { name: 'egress', data: (this.stats()?.nar_egress ?? []).map((p) => +(p.sum / GIB).toFixed(3)) },
  ]);
  workerCats = computed(() =>
    (this.stats()?.workers ?? []).map((w) => (w.worker_id ?? '-').slice(0, 12))
  );
  netSeries = computed(() => [
    { name: 'network', data: (this.stats()?.workers ?? []).map((w) => w.network_speed_mbps ?? 0) },
  ]);
  diskSeries = computed(() => [
    { name: 'disk', data: (this.stats()?.workers ?? []).map((w) => w.disk_speed_mbps ?? 0) },
  ]);

  ngOnInit(): void {
    this.board.getNetwork(24).subscribe((s) => this.stats.set(s));
  }
}
