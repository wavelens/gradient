/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DecimalPipe } from '@angular/common';
import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  inject,
  signal,
} from '@angular/core';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardFilter, TasksPage } from '@core/models';
import { ButtonComponent, StarButtonComponent } from '@shared/ui';
import { formatDuration, relativeTime } from '@shared/text';
import { EvaluationHistoryComponent } from '../evaluation-history/evaluation-history.component';
import { barsThatFit } from '../format';

const FILTERS: { key: DashboardFilter; label: string }[] = [
  { key: 'all', label: 'All' },
  { key: 'failing', label: 'Failing' },
  { key: 'worse', label: 'Got worse' },
  { key: 'starred', label: 'Starred' },
];
const HISTORY_COLUMN_PX = 210;
const TOP = 10;
const PAGE_SIZE = 25;

@Component({
  selector: 'app-dashboard-task-table',
  standalone: true,
  imports: [DecimalPipe, RouterLink, ButtonComponent, StarButtonComponent, EvaluationHistoryComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  templateUrl: './dashboard-task-table.component.html',
  styleUrl: './dashboard-task-table.component.scss',
})
export class DashboardTaskTableComponent implements OnInit {
  private dashboard = inject(DashboardService);
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private injector = inject(Injector);

  readonly filters = FILTERS;
  filter = signal<DashboardFilter>('all');
  page = signal(1);
  expanded = signal(false);
  history = signal(barsThatFit(HISTORY_COLUMN_PX));
  data = signal<TasksPage | null>(null);
  failed = signal(false);
  hidden = signal(false);

  readonly duration = formatDuration;
  readonly age = relativeTime;

  ngOnInit(): void {
    const f = this.route.snapshot.queryParamMap.get('filter');
    const known = FILTERS.find((x) => x.key === f);
    if (known) this.filter.set(known.key);
    this.load();
  }

  select(f: DashboardFilter): void {
    this.filter.set(f);
    this.page.set(1);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { filter: f === 'all' ? null : f },
      queryParamsHandling: 'merge',
    });
    this.load();
  }

  showAll(): void {
    this.expanded.set(true);
    this.page.set(1);
    this.load();
  }

  go(page: number): void {
    this.page.set(page);
    this.load();
  }

  pages(): number {
    const d = this.data();
    return d ? Math.max(1, Math.ceil(d.total / PAGE_SIZE)) : 1;
  }

  delta(d: number | null): string {
    if (d === null) return '-';
    return d > 0 ? `+${d}` : d < 0 ? String(d) : '±0';
  }

  load(): void {
    this.failed.set(false);
    const perPage = this.expanded() ? PAGE_SIZE : TOP;
    this.dashboard.tasks(this.filter(), this.page(), perPage, this.history()).subscribe({
      next: (d) => {
        this.data.set(d);
        afterNextRender(() => this.fitHistory(), { injector: this.injector });
      },
      error: (e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.failed.set(e?.status !== 403);
      },
    });
  }

  // The history column only has a width once rendered, so the first page asks for a guess and refits once.
  private fitHistory(): void {
    const width = this.host.nativeElement.querySelector('.history-col')?.clientWidth || HISTORY_COLUMN_PX;
    const fit = barsThatFit(width);
    if (fit === this.history()) return;
    this.history.set(fit);
    this.load();
  }
}
