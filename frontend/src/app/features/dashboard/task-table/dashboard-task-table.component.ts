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
import { EMPTY, Observable, Subject, catchError, debounceTime, distinctUntilChanged, filter, map, switchMap, tap } from 'rxjs';
import { DashboardService } from '@core/services/dashboard.service';
import { Filter, Latest, TaskRow, TasksPage } from '@core/models';
import { evaluationPhase, explainWaiting, waitingAccessFrom } from '@shared/evaluation';
import {
  ButtonComponent,
  EmptyStateComponent,
  IconComponent,
  LoadStateComponent,
  RowComponent,
  RowListComponent,
  SkeletonComponent,
  TabSwitchComponent,
} from '@gradient/ui/ui';
import { StatusIconComponent } from '@shared/ui';
import { formatDuration, RelativeTimePipe } from '@shared/text';
import { EvaluationHistoryComponent } from '../evaluation-history/evaluation-history.component';
import { MAX_BARS, barsThatFit } from '../format';
import { links } from '@shared/routing';

const FILTERS: { key: Filter; label: string; empty: string }[] = [
  { key: 'starred', label: 'Starred', empty: 'No starred tasks' },
  { key: 'all', label: 'All', empty: 'No tasks' },
  { key: 'failing', label: 'Failing', empty: 'No failing tasks' },
];
const HISTORY_COLUMN_PX = 150;
const TOP = 10;
const PAGE_SIZE = 25;
const RESIZE_DEBOUNCE_MS = 150;

interface DashboardTaskListRequestParameters {
  filter: Filter | null;
  page: number;
  perPage: number;
}

function parseFilter(value: string | null): Filter | null {
  return FILTERS.find((x) => x.key === value)?.key ?? null;
}

function parsePageQueryParameter(value: string | null): number | null {
  const page = Number(value);
  return value !== null && Number.isInteger(page) && page >= 1 ? page : null;
}

function evaluationNoNumbersText(t: TaskRow): string | null {
  switch (t.latest?.status) {
    case 'Aborted':
      return 'Aborted';
    case 'Waiting':
      return '';
    case 'Failed':
      return t.entry_points?.total ? null : 'Eval error';
    default:
      return null;
  }
}

function areDashboardTaskListRequestParametersEqual(a: DashboardTaskListRequestParameters, b: DashboardTaskListRequestParameters): boolean {
  return a.filter === b.filter && a.page === b.page && a.perPage === b.perPage;
}

@Component({
  selector: 'app-dashboard-task-table',
  standalone: true,
  imports: [
    RelativeTimePipe,
    FormsModule,
    ButtonComponent,
    EmptyStateComponent,
    IconComponent,
    LoadStateComponent,
    RowComponent,
    RowListComponent,
    SkeletonComponent,
    StatusIconComponent,
    TabSwitchComponent,
    EvaluationHistoryComponent,
  ],
  changeDetection: ChangeDetectionStrategy.Eager,
  templateUrl: './dashboard-task-table.component.html',
  styleUrl: './dashboard-task-table.component.scss',
})
export class DashboardTaskTableComponent implements OnInit {
  protected readonly links = links;
  private dashboard = inject(DashboardService);
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private injector = inject(Injector);
  private destroyRef = inject(DestroyRef);
  private requests = new Subject<void>();

  // null leaves the choice to the server: starred once the user starred a task, all otherwise.
  filter = signal<Filter | null>(null);
  page = signal(1);
  expanded = signal(false);
  history = signal(barsThatFit(HISTORY_COLUMN_PX));
  data = signal<TasksPage | null>(null);
  error = signal<unknown>(null);
  hidden = signal(false);
  selected = computed(() => this.filter() ?? this.data()?.filter ?? null);
  taskRowsWithNewestHistoryBarsThatFit = computed(() =>
    (this.data()?.tasks ?? []).map((t) => {
      const failedEntryPointCountText = t.entry_points?.failing ? `${t.entry_points.failing} failed` : '';
      const ratio = t.entry_points?.total ? `${t.entry_points.ok}/${t.entry_points.total}` : '';
      return {
        ...t,
        history: t.history.slice(-this.history()),
        evaluationNoNumbersText: evaluationNoNumbersText(t),
        entryPointsPassedCountText: [ratio, failedEntryPointCountText].filter(Boolean).join(' · '),
        entryPointsPassedRatioWithUnitText: [ratio && `${ratio} entry points`, failedEntryPointCountText].filter(Boolean).join(' · '),
        failedEntryPointCountText,
      };
    }),
  );
  emptyFilteredTasksTitle = computed(() => FILTERS.find((f) => f.key === this.selected())?.empty ?? 'No tasks');
  chips = computed(() => {
    const counts = this.data()?.counts;
    return FILTERS.filter((f) => f.key !== 'starred' || counts?.starred).map((f) => ({
      label: `${f.label} ${counts?.[f.key] ?? 0}`,
      value: f.key,
    }));
  });

  readonly placeholders = Array.from({ length: TOP });
  readonly duration = formatDuration;
  readonly phase = evaluationPhase;

  ngOnInit(): void {
    this.requests
      .pipe(
        switchMap(() => this.fetch()),
        takeUntilDestroyed(this.destroyRef),
      )
      .subscribe((d) => {
        this.data.set(d);
        this.leaveFilterWithoutChip(d);
        this.moveToLastPageWhenPageIsPastTheEnd();
        afterNextRender(() => this.fitHistory(), { injector: this.injector });
      });
    this.route.queryParamMap
      .pipe(
        map((q) => ({ filter: parseFilter(q.get('filter')), page: parsePageQueryParameter(q.get('page')) })),
        distinctUntilChanged((a, b) => a.filter === b.filter && a.page === b.page),
        takeUntilDestroyed(this.destroyRef),
      )
      .subscribe(({ filter, page }) => {
        this.filter.set(filter);
        this.expanded.set(page !== null);
        this.page.set(page ?? 1);
        this.load();
      });
    this.resizes()
      .pipe(debounceTime(RESIZE_DEBOUNCE_MS), takeUntilDestroyed(this.destroyRef))
      .subscribe(() => this.fitHistory());
  }

  select(f: Filter): void {
    this.router.navigate([], { relativeTo: this.route, queryParams: { filter: f, page: null }, queryParamsHandling: 'merge' });
  }

  go(page: number | null): void {
    this.router.navigate([], { relativeTo: this.route, queryParams: { page }, queryParamsHandling: 'merge' });
  }

  pages(): number {
    const d = this.data();
    return d ? Math.max(1, Math.ceil(d.total / PAGE_SIZE)) : 1;
  }

  newestEvaluationWaitingReasonTitle(project: string, latest: Latest | null | undefined): string | null {
    if (!latest || (!latest.waiting_reason && latest.status !== 'Waiting')) return null;
    return explainWaiting(latest.waiting_reason, project, waitingAccessFrom([], false, false)).title;
  }

  load(): void {
    this.requests.next();
  }

  private moveToLastPageWhenPageIsPastTheEnd(): void {
    if (!this.expanded() || this.page() <= this.pages()) return;
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { page: this.pages() },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  private leaveFilterWithoutChip(d: TasksPage): void {
    if (d.filter !== 'starred' || d.counts.starred) return;
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { filter: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  reloadSilentlyKeepingContentOnError(): Observable<TasksPage> {
    const shown = this.getCurrentTaskListRequestParameters();
    return this.requestDashboardTaskListWithFullEvaluationHistory(shown).pipe(
      filter(() => areDashboardTaskListRequestParametersEqual(shown, this.getCurrentTaskListRequestParameters())),
      tap((d) => {
        this.data.set(d);
        this.error.set(null);
      }),
    );
  }

  private getCurrentTaskListRequestParameters(): DashboardTaskListRequestParameters {
    return { filter: this.filter(), page: this.page(), perPage: this.expanded() ? PAGE_SIZE : TOP };
  }

  private requestDashboardTaskListWithFullEvaluationHistory(q: DashboardTaskListRequestParameters): Observable<TasksPage> {
    return this.dashboard.tasks(q.filter, q.page, q.perPage, MAX_BARS);
  }

  private fetch(): Observable<TasksPage> {
    this.error.set(null);
    return this.requestDashboardTaskListWithFullEvaluationHistory(this.getCurrentTaskListRequestParameters()).pipe(
      catchError((e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.error.set(e?.status === 403 ? null : e);
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
    if (width) this.history.set(barsThatFit(width));
  }
}
