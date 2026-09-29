/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { Subject, interval, startWith, switchMap } from 'rxjs';
import { BoardService, BoardStorage } from '@core/services/board.service';
import { MetricChartComponent, MetricSeries } from '@shared/ui';
import { formatCount, formatDuration } from '@shared/text';
import { alignSeries } from './storage-chart';

const WINDOWS = [1, 6, 24, 168];
const REFRESH_MS = 60_000;

type Aligned = { categories: string[]; series: MetricSeries[] };

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
    const avg = alignSeries(ops, (p) => p.avg);
    const max = alignSeries(
      ops.map((s) => ({ ...s, label: `${s.label} max` })),
      (p) => p.max
    );
    return { categories: avg.categories, series: [...avg.series, ...max.series] };
  });

  errors = computed(() => alignSeries(this.stats()?.op_errors ?? [], (p) => p.count));

  lanes = computed(() =>
    this.merge(
      alignSeries(this.stats()?.lane_fill ?? [], (p) => p.max * 100),
      alignSeries(
        (this.stats()?.send_stalls ?? []).map((s) => ({ ...s, label: `${s.label} stalls` })),
        (p) => p.count
      )
    )
  );

  serves = computed(() =>
    this.merge(
      alignSeries(this.stats()?.serve_queue ?? [], (p) => p.max),
      alignSeries(
        (this.stats()?.serve_failures ?? []).map((s) => ({ ...s, label: `failed ${s.label}` })),
        (p) => p.count
      )
    )
  );

  ngOnInit(): void {
    this.reload
      .pipe(
        switchMap(() => interval(REFRESH_MS).pipe(startWith(0))),
        switchMap(() => this.board.getStorage(this.hours())),
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
    const day = this.stats()?.granularity === 'day';
    return categories.map((c) => (day ? c.slice(5, 10) : c.slice(11, 16)));
  }

  private merge(left: Aligned, right: Aligned): Aligned {
    const categories = [...new Set([...left.categories, ...right.categories])].sort();
    const realign = (part: Aligned, axis: 'left' | 'right') =>
      part.series.map((s) => {
        const byCategory = new Map(part.categories.map((c, i) => [c, (s.data as (number | null)[])[i]]));
        return { name: s.name, axis, data: categories.map((c) => byCategory.get(c) ?? null) };
      });

    return { categories, series: [...realign(left, 'left'), ...realign(right, 'right')] };
  }
}
