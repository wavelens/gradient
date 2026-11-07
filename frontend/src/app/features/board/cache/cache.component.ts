/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { takeUntilDestroyed, toObservable } from '@angular/core/rxjs-interop';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { map, merge, switchMap, timer } from 'rxjs';
import { LiveService } from '@core/services/live.service';
import { CardGridComponent, LoadStateComponent, StatCardComponent, TabSwitchComponent } from '@gradient/ui/ui';
import { oneOf, queryParamSignal } from '@shared/routing';
import { LabelHelpComponent, MetricChartComponent } from '@shared/ui';
import { clockTime, formatBytes, formatCount, formatDuration, formatPercent } from '@shared/text';
import { BoardCacheSnapshot, BoardUpstreamCache, EventEnvelope, TrafficKind } from '@core/models';

const WINDOW_HOURS = 24;
const FIRST_FRAME_MS = 10_000;

@Component({
  selector: 'app-board-cache',
  standalone: true,
  imports: [CommonModule, FormsModule, TabSwitchComponent, CardGridComponent, StatCardComponent, MetricChartComponent, LabelHelpComponent, LoadStateComponent],
  template: `
    <gr-load-state [loading]="!stalled()" [error]="stalled()" [empty]="!snapshot()"
                   loadingMessage="Loading cache stats..." errorTitle="Failed to load cache activity" (retry)="retry()">
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
        [colors]="['--gr-chart-cyan']"
        [valueFormatter]="bytes"
      >
        <gr-tab-switch slot="actions" ariaLabel="Traffic" [options]="trafficOptions" optionValue="value"
                       [ngModel]="traffic()" (ngModelChange)="traffic.set($event)" />
      </gr-metric-chart>

      @if (busiestCats().length) {
        <gr-metric-chart
          class="busiest-caches"
          title="Busiest caches (served in 24h)"
          doc="ui/job-board/"
          type="bar"
          [horizontal]="true"
          [series]="busiestSeries()"
          [categories]="busiestCats()"
          [colors]="['--gr-chart-cyan']"
          [valueFormatter]="bytes"
        ></gr-metric-chart>
      }

      <gr-metric-chart
        title="NAR requests per hour"
        doc="ui/job-board/"
        type="line"
        [series]="requestSeries()"
        [categories]="trafficCats()"
        [colors]="['--gr-chart-purple']"
        [valueFormatter]="count"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Storage growth (added per hour)"
        doc="ui/job-board/"
        type="area"
        [series]="storageSeries()"
        [categories]="storageCats()"
        [colors]="['--gr-chart-green']"
        [valueFormatter]="bytes"
      ></gr-metric-chart>

      <h3 class="upstream-caches-title">Upstream Caches <gr-label-help doc="concepts/caches/#upstream-types" title="About upstream caches" /></h3>
      @for (u of upstreamCharts(); track u.id) {
        <gr-metric-chart
          [title]="u.title"
          type="line"
          [series]="u.series"
          [categories]="u.categories"
          [colors]="['--gr-chart-orange']"
          [valueFormatter]="duration"
        ></gr-metric-chart>
      }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache.component.scss',
})
export class BoardCacheComponent {
  private live = inject(LiveService);
  snapshot = signal<BoardCacheSnapshot | null>(null);
  stalled = signal<string | null>(null);
  private attempt = signal(0);
  readonly trafficOptions: { label: string; value: TrafficKind }[] = [
    { label: 'Total', value: 'total' },
    { label: 'External', value: 'external' },
  ];
  traffic = queryParamSignal<TrafficKind>('traffic', 'total', oneOf(this.trafficOptions.map((o) => o.value)));

  stats = computed(() => this.snapshot()?.stats);
  trafficCats = computed(() => (this.stats()?.traffic ?? []).map((p) => clockTime(p.bucket_start)));
  trafficSeries = computed(() => [
    { name: 'served', data: (this.stats()?.traffic ?? []).map((p) => p.sum) },
  ]);
  requestSeries = computed(() => [
    { name: 'requests', data: (this.stats()?.traffic ?? []).map((p) => p.count) },
  ]);
  busiestCats = computed(() => (this.stats()?.caches ?? []).map((c) => c.display_name || c.name));
  busiestSeries = computed(() => [
    { name: 'served', data: (this.stats()?.caches ?? []).map((c) => c.bytes) },
  ]);
  storageCats = computed(() => (this.stats()?.storage ?? []).map((p) => clockTime(p.bucket_start)));
  storageSeries = computed(() => [
    { name: 'added', data: (this.stats()?.storage ?? []).map((p) => p.sum) },
  ]);
  upstreamCharts = computed(() =>
    (this.snapshot()?.upstream.upstream_caches ?? []).map((u) => ({
      id: u.upstream_id,
      title: upstreamTitle(u),
      series: [{ name: 'latency', data: u.latency.map((p) => p.sum) }],
      categories: u.latency.map((p) => clockTime(p.bucket_start)),
    }))
  );

  readonly bytes = formatBytes;
  readonly count = formatCount;
  readonly duration = formatDuration;

  constructor() {
    toObservable(computed(() => ({ traffic: this.traffic(), attempt: this.attempt() })))
      .pipe(
        switchMap(({ traffic }) =>
          merge(
            this.live
              .connect<EventEnvelope<BoardCacheSnapshot>>(`/board/cache/live?window_hours=${WINDOW_HOURS}&traffic=${traffic}`)
              .pipe(map((frame) => frame.content)),
            timer(FIRST_FRAME_MS).pipe(map(() => null)),
          )
        ),
        takeUntilDestroyed()
      )
      .subscribe((content) => {
        if (content) {
          this.snapshot.set(content);
          this.stalled.set(null);
        } else if (!this.snapshot()) {
          this.stalled.set('The server sent no cache statistics in 10 seconds.');
        }
      });
  }

  retry(): void {
    this.stalled.set(null);
    this.attempt.update((n) => n + 1);
  }
}

function upstreamTitle(u: BoardUpstreamCache): string {
  const lat = u.avg_latency_ms != null ? formatDuration(u.avg_latency_ms) : 'n/a';
  const hit = u.hit_rate != null ? `${formatPercent(u.hit_rate)} hit` : 'n/a';
  return `${u.display_name} latency · ${lat} · ${hit} · ${formatCount(u.requests_total)} req`;
}
