/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, BoardWorker } from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

@Component({
  selector: 'app-board-workers',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
  template: `
    <app-metric-chart
      title="Workers by capability"
      type="bar"
      [height]="220"
      [series]="capabilitySeries()"
      [categories]="['eval', 'fetch', 'build']"
      [colors]="['#6f42c1']"
    ></app-metric-chart>

    <table class="workers">
      <thead>
        <tr><th>Worker</th><th>Org</th><th>State</th><th>Load</th><th>CPU%</th><th>RAM free</th><th>Arch</th></tr>
      </thead>
      <tbody>
        @for (w of workers(); track $index) {
          <tr>
            <td class="mono">{{ w.id ?? '—' }}</td>
            <td class="mono">{{ w.organization ?? '—' }}</td>
            <td>{{ w.draining ? 'draining' : 'active' }}</td>
            <td>{{ w.assigned_jobs }}/{{ w.max_concurrent_builds }}</td>
            <td>{{ w.cpu_usage_pct !== null ? (w.cpu_usage_pct | number: '1.0-0') : '—' }}</td>
            <td>{{ w.ram_free_mb !== null ? w.ram_free_mb + ' MB' : '—' }}</td>
            <td class="mono">{{ w.architectures.join(', ') || '—' }}</td>
          </tr>
        } @empty {
          <tr><td colspan="7" class="muted">No connected workers.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      table.workers { width: 100%; border-collapse: collapse; margin-top: 1.5rem; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .muted { color: #818181; }
    `,
  ],
})
export class BoardWorkersComponent implements OnInit {
  private board = inject(BoardService);
  workers = signal<BoardWorker[]>([]);

  capabilitySeries = computed(() => {
    const w = this.workers();
    return [
      {
        name: 'workers',
        data: [
          w.filter((x) => x.eval).length,
          w.filter((x) => x.fetch).length,
          w.filter((x) => x.build).length,
        ],
      },
    ];
  });

  ngOnInit(): void {
    this.board.getWorkers().subscribe((w) => this.workers.set(w));
  }
}
