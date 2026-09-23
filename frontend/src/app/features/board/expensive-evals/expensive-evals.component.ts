/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { LoadingSpinnerComponent, TableComponent } from '@shared/ui';
import { firstLoad } from '../first-load';
import { formatQuantity } from '@shared/text';
import { BoardService, ExpensiveEval } from '@core/services/board.service';

type Tab = 'time' | 'rss' | 'heap' | 'thunks' | 'fncalls' | 'alloc';

@Component({
  selector: 'app-board-expensive-evals',
  standalone: true,
  imports: [CommonModule, TableComponent, LoadingSpinnerComponent],
  template: `
    @if (first.loading()) {
      <gr-loading-spinner message="Loading evaluations..." />
    } @else {
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

      <gr-table class="expensive">
        <thead><tr><th>#</th><th>Evaluation</th><th>{{ valueHeader() }}</th><th>Worker</th></tr></thead>
        <tbody>
          @for (r of rows(); track r.evaluation; let i = $index) {
            <tr><td>{{ i + 1 }}</td><td class="mono">{{ r.name }}</td><td>{{ quantity(r.value, r.unit) }}</td><td class="mono">{{ r.worker || '-' }}</td></tr>
          } @empty {
            <tr><td colspan="4" class="muted">No evaluation metrics recorded in this window.</td></tr>
          }
        </tbody>
      </gr-table>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './expensive-evals.component.scss',
})
export class BoardExpensiveEvalsComponent implements OnInit {
  private board = inject(BoardService);
  protected first = firstLoad();
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

  readonly quantity = formatQuantity;

  valueHeader = computed(() => this.tabs.find((t) => t.key === this.tab())?.label ?? '');

  ngOnInit(): void {
    this.load();
  }

  private load(): void {
    this.board
      .getExpensiveEvalsByResource(this.tab(), this.windowDays)
      .pipe(this.first.track())
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
}
