/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import { EMPTY, Observable, Subject, catchError, debounceTime, distinctUntilChanged, map, switchMap } from 'rxjs';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardFilter, TasksPage } from '@core/models';
import { evaluationPhase } from '@shared/evaluation';
import {
  ButtonComponent,
  MessageBannerComponent,
  RowComponent,
  RowListComponent,
  StatusIconComponent,
  TabSwitchComponent,
} from '@shared/ui';
import { formatDuration, relativeTime } from '@shared/text';
import { EvaluationHistoryComponent } from '../evaluation-history/evaluation-history.component';
import { barsThatFit } from '../format';

const FILTERS: { key: DashboardFilter; label: string }[] = [
  { key: 'all', label: 'All' },
  { key: 'failing', label: 'Failing' },
  { key: 'starred', label: 'Starred' },
];
const HISTORY_COLUMN_PX = 210;
const TOP = 10;
const PAGE_SIZE = 25;
const RESIZE_DEBOUNCE_MS = 150;

function parseFilter(value: string | null): DashboardFilter {
  return FILTERS.find((x) => x.key === value)?.key ?? 'all';
}

@Component({
  selector: 'app-dashboard-task-table',
  standalone: true,
  imports: [
    FormsModule,
    ButtonComponent,
    MessageBannerComponent,
    RowComponent,
    RowListComponent,
    StatusIconComponent,
    TabSwitchComponent,
    EvaluationHistoryComponent,
  ],
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
  private destroyRef = inject(DestroyRef);
  private requests = new Subject<void>();

  filter = signal<DashboardFilter>('all');
  page = signal(1);
  expanded = signal(false);
  history = signal(barsThatFit(HISTORY_COLUMN_PX));
  data = signal<TasksPage | null>(null);
  failed = signal(false);
  hidden = signal(false);
  chips = computed(() =>
    FILTERS.map((f) => ({ label: `${f.label} ${this.data()?.counts?.[f.key] ?? 0}`, value: f.key })),
  );

  readonly duration = formatDuration;
  readonly age = relativeTime;
  readonly phase = evaluationPhase;

  ngOnInit(): void {
    this.requests
      .pipe(
        switchMap(() => this.fetch()),
        takeUntilDestroyed(this.destroyRef),
      )
      .subscribe((d) => {
        this.data.set(d);
        afterNextRender(() => this.fitHistory(), { injector: this.injector });
      });
    this.route.queryParamMap
      .pipe(
        map((q) => parseFilter(q.get('filter'))),
        distinctUntilChanged(),
        takeUntilDestroyed(this.destroyRef),
      )
      .subscribe((f) => {
        this.filter.set(f);
        this.page.set(1);
        this.load();
      });
    this.resizes()
      .pipe(debounceTime(RESIZE_DEBOUNCE_MS), takeUntilDestroyed(this.destroyRef))
      .subscribe(() => this.fitHistory());
  }

  select(f: DashboardFilter): void {
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { filter: f === 'all' ? null : f },
      queryParamsHandling: 'merge',
    });
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
    return d > 0 ? `+${d}` : d < 0 ? String(d) : '0';
  }

  load(): void {
    this.requests.next();
  }

  private fetch(): Observable<TasksPage> {
    this.failed.set(false);
    const perPage = this.expanded() ? PAGE_SIZE : TOP;
    return this.dashboard.tasks(this.filter(), this.page(), perPage, this.history()).pipe(
      catchError((e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.failed.set(e?.status !== 403);
        return EMPTY;
      }),
    );
  }

  private resizes(): Observable<void> {
    return new Observable<void>((sub) => {
      if (typeof ResizeObserver === 'undefined') return;
      const observer = new ResizeObserver(() => sub.next());
      observer.observe(this.host.nativeElement);
      return () => observer.disconnect();
    });
  }

  // The bar strip ignores its content width, so measuring it after render or a resize settles in one step.
  private fitHistory(): void {
    const width = this.host.nativeElement.querySelector<HTMLElement>('.history')?.clientWidth;
    if (!width) return;
    const fit = barsThatFit(width);
    if (fit === this.history()) return;
    this.history.set(fit);
    this.load();
  }
}
