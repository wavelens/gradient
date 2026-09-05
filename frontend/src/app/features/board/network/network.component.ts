/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, BoardNetworkStats, HttpRouteStat } from '@core/services/board.service';
import { LoadingSpinnerComponent, MetricChartComponent, TableComponent } from '@shared/ui';
import { firstLoad } from '../first-load';

const GIB = 1024 ** 3;

type HttpSortKey = keyof Pick<HttpRouteStat, 'method' | 'route' | 'count' | 'avg_ms' | 'errors'>;

@Component({
  selector: 'app-board-network',
  standalone: true,
  imports: [CommonModule, MetricChartComponent, TableComponent, LoadingSpinnerComponent],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading network stats..." />
    } @else {
      <gr-metric-chart
        title="NAR egress (GiB served per hour)"
        type="area"
        [series]="egressSeries()"
        [categories]="egressCats()"
        [colors]="['#17a2b8']"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Worker network speed (Mbps, latest sample)"
        type="bar"
        [series]="netSeries()"
        [categories]="workerCats()"
        [colors]="['#6f42c1']"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Worker disk speed (Mbps, latest sample)"
        type="bar"
        [series]="diskSeries()"
        [categories]="workerCats()"
        [colors]="['#fd7e14']"
      ></gr-metric-chart>

      <h2>HTTP routes @if (!stats()?.http?.length) {<span class="muted">(superuser-only)</span>}</h2>
      <gr-table class="http">
        <thead>
          <tr>
            @for (c of httpColumns; track c.key) {
              <th [class.num]="c.numeric" [attr.aria-sort]="ariaSort(c.key)">
                <button class="gr-th-sort" type="button" (click)="sortBy(c.key)">{{ c.label }}</button>
              </th>
            }
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
      </gr-table>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './network.component.scss',
})
export class BoardNetworkComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();

  protected readonly httpColumns: { key: HttpSortKey; label: string; numeric: boolean }[] = [
    { key: 'method', label: 'Method', numeric: false },
    { key: 'route', label: 'Route', numeric: false },
    { key: 'count', label: 'Requests', numeric: true },
    { key: 'avg_ms', label: 'Avg ms', numeric: true },
    { key: 'errors', label: 'Errors', numeric: true },
  ];
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

  /// The sorted column is stated on the header itself, so screen readers get
  /// the same answer the arrow gives everyone else.
  ariaSort(key: HttpSortKey): 'ascending' | 'descending' | null {
    if (this.sortKey() !== key) return null;
    return this.sortAsc() ? 'ascending' : 'descending';
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
    this.board.getNetwork(24).pipe(this.first.track()).subscribe((s) => this.stats.set(s));
  }
}
