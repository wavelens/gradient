/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Subject, catchError, interval, startWith, switchMap } from 'rxjs';
import { BoardService, BoardStorage } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/ui';
import { formatCount, formatDuration } from '@shared/text';
import { alignSeries } from './storage-chart';

const WINDOWS = [1, 6, 24, 168];
const REFRESH_MS = 60_000;

@Component({
  selector: 'app-board-storage',
  standalone: true,
  imports: [MetricChartComponent],
  template: `
    <div class="windows">
      @for (w of windows; track w) {
        <button type="button" class="window btn" [class.active]="hours() === w" (click)="select(w)">
          {{ w === 168 ? '7d' : w + 'h' }}
        </button>
      }
    </div>

    <gr-metric-chart
      title="Storage latency (avg / max)"
      type="line"
      [series]="latency().series"
      [categories]="labels(latency().categories)"
      [valueFormatter]="duration"
    ></gr-metric-chart>

    <gr-metric-chart
      title="Storage errors"
      type="bar"
      [series]="errors().series"
      [categories]="labels(errors().categories)"
      [valueFormatter]="count"
    ></gr-metric-chart>

    <gr-metric-chart
      title="Writer lanes (peak fill, send stalls)"
      type="line"
      [series]="lanes().series"
      [categories]="labels(lanes().categories)"
      [valueFormatter]="percent"
      [secondary]="{ title: 'stalls', valueFormatter: count }"
    ></gr-metric-chart>

    <gr-metric-chart
      title="NAR serves (peak waiting / active, failures)"
      type="line"
      [series]="serves().series"
      [categories]="labels(serves().categories)"
      [valueFormatter]="count"
      [secondary]="{ title: 'failures', valueFormatter: count }"
    ></gr-metric-chart>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './storage.component.scss',
})
export class BoardStorageComponent implements OnInit {
  private board = inject(BoardService);
  private destroyRef = inject(DestroyRef);
  private reload = new Subject<void>();

  readonly windows = WINDOWS;
  readonly count = formatCount;
  readonly duration = formatDuration;
  readonly percent = (v: number) => `${Math.round(v)}%`;

  hours = signal(6);
  stats = signal<BoardStorage | null>(null);

  latency = computed(() => {
    const ops = this.stats()?.op_latency ?? [];

    return alignSeries(
      { series: ops, pick: (p) => p.avg, type: 'line' },
      { series: ops, pick: (p) => p.max, type: 'line', name: (l) => `${l} max` }
    );
  });

  errors = computed(() => alignSeries({ series: this.stats()?.op_errors ?? [], pick: (p) => p.count, counts: true }));

  lanes = computed(() =>
    alignSeries(
      { series: this.stats()?.lane_fill ?? [], pick: (p) => p.max * 100, type: 'line' },
      {
        series: this.stats()?.send_stalls ?? [],
        pick: (p) => p.count,
        name: (l) => `${l} stalls`,
        axis: 'right',
        type: 'bar',
        counts: true,
      }
    )
  );

  serves = computed(() =>
    alignSeries(
      { series: this.stats()?.serve_queue ?? [], pick: (p) => p.max, type: 'line' },
      {
        series: this.stats()?.serve_failures ?? [],
        pick: (p) => p.count,
        name: (l) => `failed ${l}`,
        axis: 'right',
        type: 'bar',
        counts: true,
      }
    )
  );

  ngOnInit(): void {
    this.reload
      .pipe(
        switchMap(() => interval(REFRESH_MS).pipe(startWith(0))),
        switchMap(() => this.board.getStorage(this.hours()).pipe(catchError(() => EMPTY))),
        takeUntilDestroyed(this.destroyRef)
      )
      .subscribe((s) => this.stats.set(s));
    this.reload.next();
  }

  select(hours: number): void {
    this.hours.set(hours);
    this.reload.next();
  }

  labels(categories: string[]): string[] {
    const granularity = this.stats()?.granularity;
    const format = (c: string) => {
      if (granularity === 'day') return c.slice(5, 10);
      if (granularity === 'hour') return `${c.slice(5, 10)} ${c.slice(11, 16)}`;
      return c.slice(11, 16);
    };

    return categories.map(format);
  }
}
