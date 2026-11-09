/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { LoadStateComponent, TabSwitchComponent, TableComponent } from '@gradient/ui/ui';
import { FormsModule } from '@angular/forms';
import { oneOf, queryParamSignal } from '@shared/routing';
import { firstLoad } from '../first-load';
import { formatQuantity } from '@shared/text';
import { BoardService } from '@core/services/board.service';
import { ExpensiveEval } from '@core/models';
import { links } from '@shared/routing';

type Tab = 'time' | 'rss' | 'thunks' | 'fncalls' | 'alloc';

@Component({
  selector: 'app-board-expensive-evals',
  standalone: true,
  imports: [CommonModule, FormsModule, RouterModule, TableComponent, TabSwitchComponent, LoadStateComponent],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading evaluations..." errorTitle="Failed to load evaluations" (retry)="retry()">
      <gr-tab-switch class="ranking" ariaLabel="Ranking" [options]="tabs" optionValue="key" [ngModel]="tab()" (ngModelChange)="setTab($event)" />

      <div class="controls">
        <label>Window (days)
          <select (change)="setWindow($event)">
            @for (d of windows; track d) {
              <option [value]="d" [selected]="d === windowDays()">{{ d }}</option>
            }
          </select>
        </label>
      </div>

      <gr-table class="expensive">
        <thead><tr><th>#</th><th>Project / Task</th><th>Evaluation</th><th>{{ valueHeader() }}</th><th>Worker</th></tr></thead>
        <tbody>
          @for (r of rows(); track r.evaluation; let i = $index) {
            <tr>
              <td>{{ i + 1 }}</td>
              <td>
                <a [routerLink]="links.project(r.project_name)">{{ r.project_display_name || r.project_name }}</a>
                /
                <a [routerLink]="links.task(r.project_name, r.task_name)">{{ r.task_display_name || r.task_name }}</a>
              </td>
              <td><a class="evaluation-link" [routerLink]="links.evaluationLog(r.project_name, r.evaluation)">Open log</a></td>
              <td>{{ quantity(r.value, r.unit) }}</td>
              <td class="mono">{{ r.worker_name ?? (r.worker || '-') }}</td>
            </tr>
          } @empty {
            <tr><td colspan="5" class="muted">No evaluation metrics recorded in this window.</td></tr>
          }
        </tbody>
      </gr-table>
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './expensive-evals.component.scss',
})
export class BoardExpensiveEvalsComponent implements OnInit {
  protected readonly links = links;
  private board = inject(BoardService);
  protected first = firstLoad();
  rows = signal<ExpensiveEval[]>([]);
  tabs: { key: Tab; label: string }[] = [
    { key: 'time', label: 'Longest' },
    { key: 'rss', label: 'Peak RSS' },
    { key: 'thunks', label: 'Thunks' },
    { key: 'fncalls', label: 'Fn calls' },
    { key: 'alloc', label: 'Allocated' },
  ];
  readonly windows = [7, 30, 90];
  tab = queryParamSignal<Tab>('tab', 'time', oneOf(this.tabs.map((t) => t.key)));
  windowDays = queryParamSignal('days', 30, oneOf(this.windows));

  readonly quantity = formatQuantity;

  valueHeader = computed(() => this.tabs.find((t) => t.key === this.tab())?.label ?? '');

  ngOnInit(): void {
    this.load();
  }

  retry(): void {
    this.first.restart();
    this.load();
  }

  private load(): void {
    this.board
      .getExpensiveEvalsByResource(this.tab(), this.windowDays())
      .pipe(this.first.track())
      .subscribe((r) => this.rows.set(r));
  }

  setTab(t: Tab): void {
    this.tab.set(t);
    this.first.refresh();
    this.load();
  }

  setWindow(e: Event): void {
    this.windowDays.set(Number((e.target as HTMLSelectElement).value));
    this.first.refresh();
    this.load();
  }
}
