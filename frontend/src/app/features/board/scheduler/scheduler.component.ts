/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, MetricPoint, ScoringSummary } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-scheduler',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <div class="kpis">
      <div class="kpi"><span class="label">Scored dispatches (24h)</span><span class="value">{{ summary()?.sample_size ?? 0 }}</span></div>
      <div class="kpi"><span class="label">Avg score</span><span class="value">{{ summary()?.score_avg | number: '1.2-2' }}</span></div>
      <div class="kpi"><span class="label">Min / Max</span><span class="value sm">{{ summary()?.score_min | number: '1.1-1' }} / {{ summary()?.score_max | number: '1.1-1' }}</span></div>
    </div>

    <app-metric-chart
      title="Wait breakdown (ms, hourly avg): queue (excl. deps) vs dependency"
      type="line"
      [series]="waitSeries()"
      [categories]="waitCategories()"
      [colors]="['#17a2b8', '#fd7e14']"
    ></app-metric-chart>

    <app-metric-chart
      title="Score distribution (24h)"
      type="bar"
      [series]="histogramSeries()"
      [categories]="histogramCategories()"
      [colors]="['#6f42c1']"
    ></app-metric-chart>

    <h2>Per-rule mean contribution</h2>
    <table class="rules">
      <thead><tr><th>Rule</th><th class="num">Avg</th><th class="num">Min</th><th class="num">Max</th><th>Weight</th></tr></thead>
      <tbody>
        @for (r of ruleRows(); track r.rule) {
          <tr>
            <td class="mono">{{ r.rule }}</td>
            <td class="num" [class.neg]="r.avg < 0">{{ r.avg | number: '1.2-2' }}</td>
            <td class="num">{{ r.min | number: '1.2-2' }}</td>
            <td class="num">{{ r.max | number: '1.2-2' }}</td>
            <td class="bar-cell"><div class="bar" [class.neg]="r.avg < 0" [style.width.%]="r.share"></div></td>
          </tr>
        } @empty {
          <tr><td colspan="5" class="muted">No scored dispatches in window.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      .kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 1rem; margin-bottom: 1.5rem; }
      .kpi { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; display: flex; flex-direction: column; gap: 0.25rem; }
      .kpi .label { color: #abb0b4; font-size: 0.8rem; }
      .kpi .value { color: #fff; font-size: 1.6rem; font-weight: 600; }
      .kpi .value.sm { font-size: 1.2rem; }
      app-metric-chart { display: block; margin-bottom: 1rem; }
      h2 { color: #fff; font-size: 1.05rem; margin: 1.5rem 0 0.75rem; }
      table.rules { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .num { text-align: right; font-variant-numeric: tabular-nums; color: #28a745; }
      .num.neg { color: #dc3545; }
      .bar-cell { width: 30%; }
      .bar { height: 10px; background: #28a745; border-radius: 3px; min-width: 2px; }
      .bar.neg { background: #dc3545; }
      .muted { color: #818181; }
    `,
  ],
})
export class BoardSchedulerComponent implements OnInit {
  private board = inject(BoardService);

  private wait = signal<MetricPoint[]>([]);
  private deps = signal<MetricPoint[]>([]);
  summary = signal<ScoringSummary | null>(null);

  waitCategories = computed(() => this.wait().map((p) => p.bucket_start.slice(11, 16)));
  waitSeries = computed(() => {
    const depMap = new Map(this.deps().map((p) => [p.bucket_start, Math.round(p.avg)]));
    return [
      { name: 'queue wait (excl. deps)', data: this.wait().map((p) => Math.round(p.avg)) },
      { name: 'dependency wait', data: this.wait().map((p) => depMap.get(p.bucket_start) ?? 0) },
    ];
  });

  histogramCategories = computed(() =>
    (this.summary()?.histogram ?? []).map((b) => b.lo.toFixed(1))
  );
  histogramSeries = computed(() => [
    { name: 'dispatches', data: (this.summary()?.histogram ?? []).map((b) => b.count) },
  ]);

  ruleRows = computed(() => {
    const rules = this.summary()?.rules ?? [];
    const maxAbs = Math.max(1e-9, ...rules.map((r) => Math.abs(r.avg)));
    return rules.map((r) => ({ ...r, share: (Math.abs(r.avg) / maxAbs) * 100 }));
  });

  ngOnInit(): void {
    this.board.query('dispatch.wait_ms', 'hour').subscribe((p) => this.wait.set(p));
    this.board.query('deps.wait_ms', 'hour').subscribe((p) => this.deps.set(p));
    this.board.getScoringSummary(24).subscribe((s) => this.summary.set(s));
  }
}
