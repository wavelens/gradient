/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import {
  BoardService,
  ExpensiveBuild,
  ExpensiveResource,
  TopOrgBuildTime,
} from '@core/services/board.service';
import { MetricChartComponent } from '@shared/components/metric-chart/metric-chart.component';

type Tab = 'time' | 'ram' | 'cpu' | 'disk' | 'network';

@Component({
  selector: 'app-board-expensive-jobs',
  standalone: true,
  imports: [CommonModule, MetricChartComponent],
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
      <label><input type="checkbox" [checked]="excludeAck()" (change)="toggleAck($event)" /> Exclude acknowledged</label>
    </div>

    @if (tab() === 'time') {
      <table class="expensive">
        <thead><tr><th>#</th><th>Derivation</th><th>Build time</th><th>Worker</th></tr></thead>
        <tbody>
          @for (b of builds(); track b.build_id; let i = $index) {
            <tr><td>{{ i + 1 }}</td><td class="mono">{{ b.name }}</td><td>{{ formatMs(b.build_time_ms) }}</td><td class="mono">{{ b.worker ?? '—' }}</td></tr>
          } @empty {
            <tr><td colspan="4" class="muted">No builds in this window.</td></tr>
          }
        </tbody>
      </table>
    } @else {
      @if (tab() === 'network') {
        <p class="note">Network is a host-level peak measured during each build's window (cgroup v2 has no per-build network accounting); exact only when the build is the host's sole network user.</p>
      }
      <table class="expensive">
        <thead><tr><th>#</th><th>Derivation</th><th>{{ valueHeader() }}</th><th>Worker</th></tr></thead>
        <tbody>
          @for (r of resources(); track r.derivation; let i = $index) {
            <tr><td>{{ i + 1 }}</td><td class="mono">{{ r.name }}</td><td>{{ formatValue(r) }}</td><td class="mono">{{ r.worker || '—' }}</td></tr>
          } @empty {
            <tr><td colspan="4" class="muted">No per-build metrics recorded in this window (needs cgroup metrics enabled on workers).</td></tr>
          }
        </tbody>
      </table>
    }

    @if (topOrgs().length) {
      <h2>Top organizations by build time (superuser)</h2>
      <app-metric-chart
        type="bar"
        [horizontal]="true"
        [height]="320"
        [series]="topOrgSeries()"
        [categories]="topOrgCategories()"
        [colors]="['#fd7e14']"
      ></app-metric-chart>
    }
  `,
  styles: [
    `
      .tabs { display: flex; gap: 0.5rem; margin-bottom: 1rem; flex-wrap: wrap; }
      .tabs button { background: #21262d; color: #abb0b4; border: 1px solid #2d333b; border-radius: 6px; padding: 0.35rem 0.8rem; cursor: pointer; }
      .tabs button.active { color: #fff; border-color: #17a2b8; }
      .controls { display: flex; gap: 1.5rem; margin-bottom: 1rem; color: #abb0b4; align-items: center; }
      select { background: #21262d; color: #fff; border: 1px solid #2d333b; border-radius: 4px; padding: 0.25rem; }
      .note { color: #818181; font-size: 0.8rem; margin: 0 0 0.75rem; }
      table.expensive { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; color: #abb0b4; font-size: 0.85rem; }
      th { color: #fff; }
      .mono { font-family: monospace; }
      .muted { color: #818181; }
      h2 { color: #fff; font-size: 1.05rem; margin: 1.5rem 0 0.75rem; }
      app-metric-chart { display: block; }
    `,
  ],
})
export class BoardExpensiveJobsComponent implements OnInit {
  private board = inject(BoardService);
  builds = signal<ExpensiveBuild[]>([]);
  resources = signal<ExpensiveResource[]>([]);
  topOrgs = signal<TopOrgBuildTime[]>([]);
  excludeAck = signal(true);
  tab = signal<Tab>('time');
  private windowDays = 30;

  tabs: { key: Tab; label: string }[] = [
    { key: 'time', label: 'Longest' },
    { key: 'ram', label: 'Peak RAM' },
    { key: 'cpu', label: 'CPU time' },
    { key: 'disk', label: 'Disk I/O' },
    { key: 'network', label: 'Network' },
  ];

  valueHeader = computed(() => this.tabs.find((t) => t.key === this.tab())?.label ?? '');
  topOrgCategories = computed(() => this.topOrgs().map((o) => o.organization.slice(0, 8)));
  topOrgSeries = computed(() => [
    { name: 'build hours', data: this.topOrgs().map((o) => +(o.total_build_ms / 3_600_000).toFixed(2)) },
  ]);

  ngOnInit(): void {
    this.load();
  }

  private load(): void {
    if (this.tab() === 'time') {
      this.board.getExpensive(this.windowDays, this.excludeAck()).subscribe((b) => this.builds.set(b));
    } else {
      this.board
        .getExpensiveByResource(
          this.tab() as 'ram' | 'cpu' | 'disk' | 'network',
          this.windowDays,
          this.excludeAck()
        )
        .subscribe((r) => this.resources.set(r));
    }
    this.board.getTopOrgs(this.windowDays).subscribe({
      next: (o) => this.topOrgs.set(o),
      error: () => this.topOrgs.set([]),
    });
  }

  setTab(t: Tab): void {
    this.tab.set(t);
    this.load();
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

  formatValue(r: ExpensiveResource): string {
    if (r.unit === 'bytes') {
      const gib = r.value / 1024 ** 3;
      return gib >= 1 ? `${gib.toFixed(2)} GiB` : `${(r.value / 1024 ** 2).toFixed(1)} MiB`;
    }
    if (r.unit === 'ms') return this.formatMs(r.value);
    if (r.unit === 'MB') return `${(r.value / 1024).toFixed(2)} GiB`;
    return `${r.value.toFixed(1)} ${r.unit}`;
  }
}
