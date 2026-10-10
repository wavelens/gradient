/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, MetricPoint, RuleDescription, ScoringSummary } from '@core/services/board.service';
import { CardGridComponent, LoadingSpinnerComponent, PopoverComponent, TableComponent, StatCardComponent } from '@gradient/ui/ui';
import { LabelHelpComponent, MetricChartComponent } from '@shared/ui';
import { firstLoad } from '../first-load';
import { clockTime, formatDuration } from '@shared/text';

@Component({
  selector: 'app-board-scheduler',
  standalone: true,
  imports: [CommonModule, CardGridComponent, StatCardComponent, PopoverComponent, MetricChartComponent, LabelHelpComponent, TableComponent, LoadingSpinnerComponent],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading scheduler stats..." />
    } @else {
      <gr-card-grid class="kpis" min="180px">
        <gr-stat-card compact label="Scored assignments (24h)" [value]="summary()?.sample_size ?? 0" />
        <gr-stat-card compact label="Avg score" [value]="summary()?.score_avg | number: '1.2-2'" />
        <gr-stat-card compact label="Min / Max" [value]="(summary()?.score_min | number: '1.1-1') + ' / ' + (summary()?.score_max | number: '1.1-1')" />
      </gr-card-grid>

      <gr-metric-chart
        title="Wait breakdown (hourly avg): queue (excl. deps) vs dependency"
        doc="ui/job-board/"
        type="line"
        [series]="waitSeries()"
        [categories]="waitCategories()"
        [colors]="['#17a2b8', '#fd7e14']"
        [valueFormatter]="duration"
      ></gr-metric-chart>

      <gr-metric-chart
        title="Score distribution (24h)"
        doc="reference/scheduler-policies/"
        type="bar"
        [series]="histogramSeries()"
        [categories]="histogramCategories()"
        [colors]="['#6f42c1']"
      ></gr-metric-chart>

      <h2>Per-rule mean contribution <gr-label-help doc="reference/scheduler-policies/" title="About the scoring rules" /></h2>
      <gr-table class="rules">
        <thead><tr><th>Rule</th><th class="num">Avg</th><th class="num">Min</th><th class="num">Max</th><th>Weight</th></tr></thead>
        <tbody>
          @for (r of ruleRows(); track r.rule) {
            <tr>
              <td class="mono">
                {{ r.rule }}
                @if (r.description) {
                  <button type="button" class="help" aria-label="Explain rule" (click)="showHelp($event, r, rulePop)">?</button>
                }
              </td>
              <td class="num" [class.neg]="r.avg < 0">{{ r.avg | number: '1.2-2' }}</td>
              <td class="num">{{ r.min | number: '1.2-2' }}</td>
              <td class="num">{{ r.max | number: '1.2-2' }}</td>
              <td class="bar-cell"><div class="bar" [class.neg]="r.avg < 0" [style.width.%]="r.share"></div></td>
            </tr>
          } @empty {
            <tr><td colspan="5" class="muted">No scored assignments in window.</td></tr>
          }
        </tbody>
      </gr-table>

      <gr-popover #rulePop>
        @if (activeRule(); as a) {
          <div class="rule-help"><strong>{{ a.rule }}</strong><p>{{ a.description }}</p></div>
        }
      </gr-popover>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './scheduler.component.scss',
})
export class BoardSchedulerComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();

  private wait = signal<MetricPoint[]>([]);
  private deps = signal<MetricPoint[]>([]);
  summary = signal<ScoringSummary | null>(null);
  private descriptions = signal<Map<string, string>>(new Map());
  activeRule = signal<RuleDescription | null>(null);

  readonly duration = formatDuration;

  waitCategories = computed(() => this.wait().map((p) => clockTime(p.bucket_start)));
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
    { name: 'assignments', data: (this.summary()?.histogram ?? []).map((b) => b.count) },
  ]);

  ruleRows = computed(() => {
    const rules = this.summary()?.rules ?? [];
    const descriptions = this.descriptions();
    const maxAbs = Math.max(1e-9, ...rules.map((r) => Math.abs(r.avg)));
    return rules.map((r) => ({
      ...r,
      share: (Math.abs(r.avg) / maxAbs) * 100,
      description: descriptions.get(r.rule) ?? '',
    }));
  });

  showHelp(event: Event, row: { rule: string; description: string }, popover: PopoverComponent): void {
    this.activeRule.set({ rule: row.rule, description: row.description });
    popover.toggle(event);
  }

  ngOnInit(): void {
    this.board.query('dispatch.wait_ms', 'hour').pipe(this.first.track()).subscribe((p) => this.wait.set(p));
    this.board.query('deps.wait_ms', 'hour').pipe(this.first.track()).subscribe((p) => this.deps.set(p));
    this.board.getScoringSummary(24).pipe(this.first.track()).subscribe((s) => this.summary.set(s));
    this.board
      .getScoringRules()
      .pipe(this.first.track())
      .subscribe((rules) => this.descriptions.set(new Map(rules.map((r) => [r.rule, r.description]))));
  }
}
