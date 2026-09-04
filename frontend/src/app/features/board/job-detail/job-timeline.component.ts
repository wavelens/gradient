/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, OnDestroy, afterNextRender, computed, effect, inject, input, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import * as echarts from 'echarts/core';
import { CustomChart } from 'echarts/charts';
import { GridComponent, TooltipComponent } from 'echarts/components';
import { SVGRenderer } from 'echarts/renderers';
import { JobPhase } from '@core/services/board.service';
import { EmptyStateComponent } from '@shared/ui';
import { resolveChartTheme } from '@shared/ui/metric-chart/metric-chart.component';
import { ThemeService } from '@core/services/theme.service';

echarts.use([CustomChart, GridComponent, TooltipComponent, SVGRenderer]);

/// How deeply each span is nested, by walking `parent_seq` to the root.
export function depths(phases: JobPhase[]): number[] {
  const bySeq = new Map(phases.map(p => [p.seq, p]));
  return phases.map(p => {
    let depth = 0;
    let cur = p;
    while (cur.parent_seq !== null) {
      const parent = bySeq.get(cur.parent_seq);
      if (!parent || parent === cur) { break; }
      cur = parent;
      depth += 1;
    }
    return depth;
  });
}

export interface PhaseRow {
  seq: number;
  phase: string;
  depth: number;
  startMs: number;
  durationMs: number;
  /// Fraction of the job's whole span, so the bars and the table agree.
  share: number;
  paths: number;
  bytes: number;
}

export function phaseRows(phases: JobPhase[]): PhaseRow[] {
  if (phases.length === 0) { return []; }

  const depth = depths(phases);
  const total = Math.max(...phases.map(p => p.end_ms), 1);
  return phases.map((p, i) => ({
    seq: p.seq,
    phase: p.phase,
    depth: depth[i],
    startMs: p.start_ms,
    durationMs: p.end_ms - p.start_ms,
    share: (p.end_ms - p.start_ms) / total,
    paths: p.paths,
    bytes: p.bytes,
  }));
}

const PHASE_LABELS: Record<string, string> = {
  fetch: 'Fetch',
  push_inputs: 'Push inputs',
  eval_flake: 'Eval flake',
  eval_derivations: 'Eval derivations',
  eval_cache_pull: 'Eval-cache pull',
  eval_cache_push: 'Eval-cache push',
  known_derivations_wait: 'Known-derivations wait',
  drv_closure_push: 'Drv-closure push',
  prefetch: 'Prefetch',
  substitute_relay: 'Substitute relay',
  build: 'Build',
  compress: 'Compress',
  nar_push: 'NAR push',
  cache_query_wait: 'Cache-query wait',
};

export function phaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? phase;
}

@Component({
  selector: 'gr-job-timeline',
  standalone: true,
  imports: [CommonModule, EmptyStateComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (rows().length) {
      <div class="chart" #chart [style.height.px]="chartHeight()"></div>

      <table class="phases">
        <thead>
          <tr><th>Phase</th><th class="num">Start</th><th class="num">Duration</th><th>Share</th><th class="num">Paths</th><th class="num">Bytes</th></tr>
        </thead>
        <tbody>
          @for (r of rows(); track r.seq) {
            <tr>
              <td class="name" [style.padding-left.px]="8 + r.depth * 16">{{ label(r.phase) }}</td>
              <td class="num mono">{{ r.startMs | number }} ms</td>
              <td class="num mono">{{ r.durationMs | number }} ms</td>
              <td class="bar-cell"><div class="bar" [style.width.%]="r.share * 100"></div></td>
              <td class="num mono">{{ r.paths || '-' }}</td>
              <td class="num mono">{{ r.bytes ? (r.bytes | number) : '-' }}</td>
            </tr>
          }
        </tbody>
      </table>
    } @else {
      <gr-empty-state icon="schedule" title="No timeline" message="This job reported no phase spans." flat />
    }
  `,
  styles: [`
    .chart { width: 100%; }
    table.phases { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
    table.phases th { text-align: left; color: var(--gr-text-secondary); font-weight: 500; padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--gr-border); }
    table.phases td { padding: 0.35rem 0.5rem; border-bottom: 1px solid var(--gr-border); }
    .num { text-align: right; }
    .mono { font-family: var(--gr-font-mono); }
    .bar-cell { width: 30%; }
    .bar { height: 8px; border-radius: 2px; background: var(--gr-graph-running); min-width: 1px; }
  `],
})
export class JobTimelineComponent implements OnDestroy {
  readonly phases = input.required<JobPhase[]>();

  private readonly chartRef = viewChild<ElementRef<HTMLElement>>('chart');
  private readonly theme = inject(ThemeService);
  private chart: echarts.ECharts | null = null;

  readonly rows = computed(() => phaseRows(this.phases()));
  readonly chartHeight = computed(() => Math.min(24 * this.rows().length + 48, 520));

  constructor() {
    afterNextRender(() => this.render());
    effect(() => {
      this.rows();
      this.theme.resolved();
      this.render();
    });
  }

  ngOnDestroy(): void {
    this.chart?.dispose();
    this.chart = null;
  }

  label(phase: string): string {
    return phaseLabel(phase);
  }

  private render(): void {
    const host = this.chartRef()?.nativeElement;
    const rows = this.rows();
    if (!host || rows.length === 0) { return; }

    this.chart ??= echarts.init(host, undefined, { renderer: 'svg' });
    const t = resolveChartTheme();
    const total = Math.max(...rows.map(r => r.startMs + r.durationMs), 1);

    this.chart.setOption({
      grid: { left: 160, right: 24, top: 8, bottom: 28, containLabel: false },
      xAxis: {
        type: 'value',
        min: 0,
        max: total,
        axisLabel: { color: t.text, formatter: (v: number) => `${v} ms` },
        splitLine: { lineStyle: { color: t.grid } },
      },
      yAxis: {
        type: 'category',
        inverse: true,
        data: rows.map(r => `${' '.repeat(r.depth * 2)}${phaseLabel(r.phase)}`),
        axisLabel: { color: t.text, fontFamily: t.mono },
        axisLine: { lineStyle: { color: t.border } },
        splitLine: { show: false },
      },
      tooltip: {
        backgroundColor: t.surface,
        borderColor: t.border,
        textStyle: { color: t.textStrong },
        formatter: (p: { dataIndex: number }) => {
          const r = rows[p.dataIndex];
          const detail = [
            r.paths ? `${r.paths} paths` : null,
            r.bytes ? `${r.bytes} bytes` : null,
          ].filter(Boolean).join(', ');
          return `${phaseLabel(r.phase)}<br/>${r.durationMs} ms at +${r.startMs} ms${detail ? `<br/>${detail}` : ''}`;
        },
      },
      series: [{
        type: 'custom',
        renderItem: (params: { dataIndex: number }, api: {
          value: (i: number) => number;
          coord: (p: [number, number]) => [number, number];
          size: (p: [number, number]) => [number, number];
          style: (s: Record<string, unknown>) => Record<string, unknown>;
        }) => {
          const idx = api.value(0);
          const start = api.coord([api.value(1), idx]);
          const end = api.coord([api.value(2), idx]);
          const height = (api.size([0, 1])[1] as number) * 0.6;
          const width = Math.max(end[0] - start[0], 1);
          return {
            type: 'rect',
            shape: { x: start[0], y: start[1] - height / 2, width, height },
            style: api.style({ fill: t.palette[rows[params.dataIndex].depth % t.palette.length] }),
          };
        },
        encode: { x: [1, 2], y: 0 },
        data: rows.map((r, i) => [i, r.startMs, r.startMs + r.durationMs]),
      }],
    }, true);
    this.chart.resize();
  }
}
