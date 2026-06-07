/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, ExpensiveBuild } from '@core/services/board.service';

@Component({
  selector: 'app-board-expensive-jobs',
  standalone: true,
  imports: [CommonModule],
  template: `
    <div class="controls">
      <label>Window (days)
        <select (change)="setWindow($event)">
          <option value="7">7</option>
          <option value="30" selected>30</option>
          <option value="90">90</option>
        </select>
      </label>
      <label><input type="checkbox" [checked]="excludeAck()" (change)="toggleAck($event)" /> Exclude acknowledged</label>
    </div>

    <table class="expensive">
      <thead>
        <tr><th>#</th><th>Derivation</th><th>Build time</th><th>Worker</th></tr>
      </thead>
      <tbody>
        @for (b of builds(); track b.build_id; let i = $index) {
          <tr>
            <td>{{ i + 1 }}</td>
            <td class="mono">{{ b.name }}</td>
            <td>{{ formatMs(b.build_time_ms) }}</td>
            <td class="mono">{{ b.worker ?? '—' }}</td>
          </tr>
        } @empty {
          <tr><td colspan="4" class="muted">No builds in this window.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      .controls { display: flex; gap: 1.5rem; margin-bottom: 1rem; color: #abb0b4; align-items: center; }
      select { background: #21262d; color: #fff; border: 1px solid #2d333b; border-radius: 4px; padding: 0.25rem; }
      table.expensive { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .muted { color: #818181; }
    `,
  ],
})
export class BoardExpensiveJobsComponent implements OnInit {
  private board = inject(BoardService);
  builds = signal<ExpensiveBuild[]>([]);
  excludeAck = signal(true);
  private windowDays = 30;

  ngOnInit(): void {
    this.load();
  }

  private load(): void {
    this.board.getExpensive(this.windowDays, this.excludeAck()).subscribe((b) => this.builds.set(b));
  }

  setWindow(e: Event): void {
    this.windowDays = Number((e.target as HTMLSelectElement).value);
    this.load();
  }

  toggleAck(e: Event): void {
    this.excludeAck.set((e.target as HTMLInputElement).checked);
    this.load();
  }

  formatMs(ms: number): string {
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
  }
}
