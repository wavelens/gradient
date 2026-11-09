/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { BoardService } from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';
import { LoadStateComponent, TabSwitchComponent, TableComponent } from '@gradient/ui/ui';
import { FormsModule } from '@angular/forms';
import { LabelHelpComponent, MetricChartComponent } from '@shared/ui';
import { RouterModule } from '@angular/router';
import { links, oneOf, queryParamSignal } from '@shared/routing';
import { firstLoad, requestFailedText } from '../first-load';
import { BoardProjectTaskLinksComponent } from '../project-task-links.component';
import { formatDuration, formatQuantity } from '@shared/text';
import { ExpensiveBuild, ExpensiveResource, TopProjectBuildTime } from '@core/models';

type Tab = 'time' | 'ram' | 'cpu' | 'disk';

@Component({
  selector: 'app-board-expensive-jobs',
  standalone: true,
  imports: [CommonModule, FormsModule, RouterModule, MetricChartComponent, LabelHelpComponent, TableComponent, TabSwitchComponent, LoadStateComponent, BoardProjectTaskLinksComponent],
  template: `
    <gr-load-state [loading]="first.loading()" [error]="first.error()" [empty]="!first.received()"
                   loadingMessage="Loading jobs..." errorTitle="Failed to load jobs" (retry)="retry()">
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

      @if (tab() === 'time') {
        <gr-table class="expensive">
          <thead><tr><th>#</th><th>Project / task</th><th>Derivation</th><th>Build time</th><th>Worker</th></tr></thead>
          <tbody>
            @for (b of builds() ?? []; track b.build_id; let i = $index) {
              <tr><td>{{ i + 1 }}</td>
                <td><app-board-project-task-links [project]="b.project_slug" [projectDisplayName]="b.project_display_name"
                                                  [task]="b.task_name" [taskDisplayName]="b.task_display_name" [evaluation]="b.evaluation" /></td>
                <td class="mono"><a class="build-link" [routerLink]="links.evaluationLog(b.project_slug, b.evaluation, { build: b.build_id })">{{ b.name }}</a></td><td>{{ duration(b.build_time_ms) }}</td><td class="mono">{{ b.worker_name ?? b.worker ?? '-' }}</td></tr>
            } @empty {
              <tr><td colspan="5" class="muted">{{ builds() === null ? failed : 'No builds in this window.' }}</td></tr>
            }
          </tbody>
        </gr-table>
      } @else {
        <gr-table class="expensive">
          <thead><tr><th>#</th><th>Derivation</th><th>{{ valueHeader() }}</th><th>Worker</th></tr></thead>
          <tbody>
            @for (r of resources(); track r.derivation; let i = $index) {
              <tr><td>{{ i + 1 }}</td><td class="mono"><a class="build-link" [routerLink]="links.evaluationLog(r.project_slug, r.evaluation)">{{ r.name }}</a></td><td>{{ quantity(r.value, r.unit) }}</td><td class="mono">{{ r.worker_name ?? (r.worker || '-') }}</td></tr>
            } @empty {
              <tr><td colspan="4" class="muted">No per-build metrics recorded in this window (needs build metrics enabled on workers).</td></tr>
            }
          </tbody>
        </gr-table>
      }

      @if (tab() === 'time' && auth.user()?.superuser && topProjects()?.length !== 0) {
        <h2>Top projects by build time <gr-label-help doc="ui/job-board/" title="About the Job Board" /></h2>
        <gr-metric-chart
          type="bar"
          [horizontal]="true"
          [height]="320"
          [series]="topProjectSeries()"
          [categories]="topProjectCategories()"
          [colors]="['--gr-chart-orange']"
          [valueFormatter]="duration"
          [error]="topProjects() === null"
        ></gr-metric-chart>
      }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './expensive-jobs.component.scss',
})
export class BoardExpensiveJobsComponent implements OnInit {
  protected readonly links = links;
  private board = inject(BoardService);
  protected auth = inject(AuthService);
  protected first = firstLoad();
  protected readonly failed = requestFailedText;
  builds = signal<ExpensiveBuild[] | null>(null);
  resources = signal<ExpensiveResource[]>([]);
  topProjects = signal<TopProjectBuildTime[] | null>(null);
  tabs: { key: Tab; label: string }[] = [
    { key: 'time', label: 'Longest' },
    { key: 'ram', label: 'Peak RAM' },
    { key: 'cpu', label: 'CPU time' },
    { key: 'disk', label: 'Disk I/O' },
  ];
  readonly windows = [7, 30, 90];
  tab = queryParamSignal<Tab>('tab', 'time', oneOf(this.tabs.map((t) => t.key)));
  windowDays = queryParamSignal('days', 30, oneOf(this.windows));

  readonly duration = formatDuration;
  readonly quantity = formatQuantity;

  valueHeader = computed(() => this.tabs.find((t) => t.key === this.tab())?.label ?? '');
  topProjectCategories = computed(() => (this.topProjects() ?? []).map((o) => o.project_display_name || o.project_name));
  topProjectSeries = computed(() => [
    { name: 'build time', data: (this.topProjects() ?? []).map((o) => o.total_build_ms) },
  ]);

  ngOnInit(): void {
    this.load();
  }

  retry(): void {
    this.first.restart();
    this.load();
  }

  private load(): void {
    if (this.tab() === 'time') {
      this.board.getExpensive(this.windowDays()).pipe(this.first.track()).subscribe((b) => this.builds.set(b));
      if (this.auth.user()?.superuser === true) {
        this.board.getTopProjects(this.windowDays()).pipe(this.first.track()).subscribe((o) => this.topProjects.set(o));
      }
    } else {
      this.board
        .getExpensiveByResource(this.tab() as 'ram' | 'cpu' | 'disk', this.windowDays())
        .pipe(this.first.track())
        .subscribe((r) => this.resources.set(r));
    }
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
