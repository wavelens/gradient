/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, BoardHealth } from '@core/services/board.service';

const MIB = 1024 ** 2;

@Component({
  selector: 'app-board-health',
  standalone: true,
  imports: [CommonModule],
  template: `
    @if (health(); as h) {
      <div class="kpis">
        <div class="kpi"><span class="label">Version</span><span class="value sm">{{ h.version }}</span></div>
        <div class="kpi"><span class="label">Uptime</span><span class="value sm">{{ uptime(h.uptime_seconds) }}</span></div>
        <div class="kpi"><span class="label">Workers</span><span class="value">{{ h.workers_connected }}</span></div>
        <div class="kpi"><span class="label">Jobs pending / active</span><span class="value sm">{{ h.jobs_pending }} / {{ h.jobs_active }}</span></div>
      </div>

      <h2>Process</h2>
      <div class="grid">
        <div class="cell"><span class="label">RSS</span><span>{{ mib(h.process.resident_memory_bytes) }} MiB</span></div>
        <div class="cell"><span class="label">Virtual</span><span>{{ mib(h.process.virtual_memory_bytes) }} MiB</span></div>
        <div class="cell"><span class="label">Open fds</span><span>{{ h.process.open_fds }} / {{ h.process.max_fds }}</span></div>
        <div class="cell"><span class="label">Threads</span><span>{{ h.process.threads }}</span></div>
        <div class="cell"><span class="label">CPU seconds</span><span>{{ h.process.cpu_seconds_total | number: '1.0-0' }}</span></div>
      </div>

      <h2>Pipeline</h2>
      <div class="grid">
        <div class="cell"><span class="label">Rollup lag</span><span [class.bad]="(h.rollup_lag_seconds ?? 0) > 300">{{ h.rollup_lag_seconds !== null ? (h.rollup_lag_seconds | number: '1.0-0') + ' s' : 'no data' }}</span></div>
        <div class="cell"><span class="label">Latest bucket</span><span>{{ h.latest_rollup_bucket ? (h.latest_rollup_bucket | date: 'short') : '—' }}</span></div>
        <div class="cell"><span class="label">Cache size</span><span>{{ (h.cache_bytes / (1024*1024*1024)) | number: '1.2-2' }} GiB</span></div>
        <div class="cell"><span class="label">Packages</span><span>{{ h.cache_packages }}</span></div>
      </div>

      <h2>HTTP routes</h2>
      <table class="http">
        <thead><tr><th>Method</th><th>Route</th><th class="num">Requests</th><th class="num">Avg ms</th><th class="num">Errors</th></tr></thead>
        <tbody>
          @for (r of h.http; track r.method + r.route) {
            <tr>
              <td>{{ r.method }}</td>
              <td class="mono">{{ r.route }}</td>
              <td class="num">{{ r.count }}</td>
              <td class="num">{{ r.avg_ms | number: '1.1-1' }}</td>
              <td class="num" [class.bad]="r.errors > 0">{{ r.errors }}</td>
            </tr>
          } @empty {
            <tr><td colspan="5" class="muted">No HTTP route data yet.</td></tr>
          }
        </tbody>
      </table>
    } @else {
      <p class="muted">Loading… (superuser only)</p>
    }
  `,
  styles: [
    `
      .kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 1rem; margin-bottom: 1rem; }
      .kpi { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; display: flex; flex-direction: column; gap: 0.25rem; }
      .kpi .label, .cell .label { color: #abb0b4; font-size: 0.8rem; }
      .kpi .value { color: #fff; font-size: 1.6rem; font-weight: 600; }
      .kpi .value.sm { font-size: 1.05rem; }
      h2 { color: #fff; font-size: 1.05rem; margin: 1.5rem 0 0.75rem; }
      .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 1rem; }
      .cell { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 0.75rem; display: flex; flex-direction: column; gap: 0.25rem; color: #d6dade; font-size: 0.95rem; }
      .cell .bad, .num.bad { color: #dc3545; }
      table.http { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .num { text-align: right; font-variant-numeric: tabular-nums; }
      .muted { color: #818181; }
    `,
  ],
})
export class BoardHealthComponent implements OnInit {
  private board = inject(BoardService);
  health = signal<BoardHealth | null>(null);

  mib(bytes: number): string {
    return (bytes / MIB).toFixed(0);
  }

  uptime(seconds: number): string {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : `${m}m`;
  }

  ngOnInit(): void {
    this.board.getHealth().subscribe((h) => this.health.set(h));
  }
}
