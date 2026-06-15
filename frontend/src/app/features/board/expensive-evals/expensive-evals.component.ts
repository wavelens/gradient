/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService, ExpensiveEval } from '@core/services/board.service';

type Tab = 'time' | 'rss' | 'heap' | 'thunks' | 'fncalls' | 'alloc';

@Component({
  selector: 'app-board-expensive-evals',
  standalone: true,
  imports: [CommonModule],
  template: `
    <nav class="tabs">
      @for (t of tabs; track t.key) {
        <button [class.active]="tab() === t.key" (click)="setTab(t.key)">{{ t.label }}</button>
      }
    </nav>

    <div class="controls">
      <label>Window (days)
        <select (change)="setWindow($event)">
          <option value="7">7</option>
          <option value="30" selected>30</option>
          <option value="90">90</option>
        </select>
      </label>
    </div>

    <table class="expensive">
      <thead><tr><th>#</th><th>Evaluation</th><th>{{ valueHeader() }}</th><th>Worker</th></tr></thead>
      <tbody>
        @for (r of rows(); track r.evaluation; let i = $index) {
          <tr><td>{{ i + 1 }}</td><td class="mono">{{ r.name }}</td><td>{{ formatValue(r) }}</td><td class="mono">{{ r.worker || '—' }}</td></tr>
        } @empty {
          <tr><td colspan="4" class="muted">No evaluation metrics recorded in this window.</td></tr>
        }
      </tbody>
    </table>
  `,
  styles: [
    `
      .tabs { display: flex; gap: 0.5rem; margin-bottom: 1rem; flex-wrap: wrap; }
      .tabs button { background: #21262d; color: #abb0b4; border: 1px solid #2d333b; border-radius: 6px; padding: 0.35rem 0.8rem; cursor: pointer; }
      .tabs button.active { color: #fff; border-color: #17a2b8; }
      .controls { display: flex; gap: 1.5rem; margin-bottom: 1rem; color: #abb0b4; align-items: center; }
      .controls label { display: inline-flex; align-items: center; gap: 0.35rem; cursor: pointer; }
      .help { font-size: 1rem; color: #818181; cursor: help; }
      select { background: #21262d; color: #fff; border: 1px solid #2d333b; border-radius: 4px; padding: 0.25rem; }
      .note { color: #818181; font-size: 0.8rem; margin: 0 0 0.75rem; }
      table.expensive { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .muted { color: #818181; }
      h2 { color: #fff; font-size: 1.05rem; margin: 1.5rem 0 0.75rem; }
    `,
  ],
})
export class BoardExpensiveEvalsComponent implements OnInit {
  private board = inject(BoardService);
  rows = signal<ExpensiveEval[]>([]);
  tab = signal<Tab>('time');
  private windowDays = 30;

  tabs: { key: Tab; label: string }[] = [
    { key: 'time', label: 'Slowest' },
    { key: 'rss', label: 'Peak RSS' },
    { key: 'heap', label: 'Peak heap' },
    { key: 'thunks', label: 'Thunks' },
    { key: 'fncalls', label: 'Fn calls' },
    { key: 'alloc', label: 'Allocated' },
  ];

  valueHeader = computed(() => this.tabs.find((t) => t.key === this.tab())?.label ?? '');

  ngOnInit(): void {
    this.load();
  }

  private load(): void {
    this.board
      .getExpensiveEvalsByResource(this.tab(), this.windowDays)
      .subscribe((r) => this.rows.set(r));
  }

  setTab(t: Tab): void {
    this.tab.set(t);
    this.load();
  }

  setWindow(e: Event): void {
    this.windowDays = Number((e.target as HTMLSelectElement).value);
    this.load();
  }

  formatMs(ms: number): string {
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
  }

  formatValue(r: ExpensiveEval): string {
    if (r.unit === 'ms') return this.formatMs(r.value);
    if (r.unit === 'MB') return `${(r.value / 1024).toFixed(2)} GiB`;
    if (r.unit === 'bytes') {
      const gib = r.value / 1024 ** 3;
      return gib >= 1 ? `${gib.toFixed(2)} GiB` : `${(r.value / 1024 ** 2).toFixed(1)} MiB`;
    }
    if (r.unit === 'count') return r.value.toLocaleString();
    return `${r.value.toFixed(1)} ${r.unit}`;
  }
}
