/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { EMPTY, Observable, Subject, Subscription, auditTime, catchError, filter, forkJoin, of, switchMap, tap } from 'rxjs';
import { LiveEvent, LiveService, createResyncOnLiveSocketReconnectCallback } from '@core/services/live.service';
import { EvaluationWithOptionalTask } from '@core/resolvers/evaluation-access.resolver';
import type {
  FailedBuildSummary,
  EvaluationResponse,
  EvaluationFailureSummary,
  TaskDetailsResponse,
} from '@core/models';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { AuthService } from '@core/services/auth.service';
import { accessFromEntity } from '@core/models/access.model';
import { links } from '@shared/routing';
import { EvaluationsService } from '@core/services/evaluations.service';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  EmptyStateComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  StatCardComponent,
} from '@gradient/ui/ui';
import {
  commitLabel,
  commitWebUrl,
  evaluationDuration,
  isRunningEvaluationStatus,
  repositoryWebUrl,
  waitingAccessFrom,
} from '@shared/evaluation';
import { CurrentTimeSignal, formatDuration, plural, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { EvalStatusBadgeComponent, LiveStatusComponent, StatusIconComponent, WaitingNoticeComponent } from '@shared/ui';
import { blockedLine, failureKind, failureTitle, firstLine, getEvaluationSummaryCountTiles, headline } from './summary-text';

const SUMMARY_REFRESH_MS = 1000;

interface FailureGroup {
  title: string;
  failures: FailedBuildSummary[];
}

@Component({
  selector: 'app-evaluation-summary',
  standalone: true,
  imports: [
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
    RouterModule,
    BadgeComponent,
    ButtonComponent,
    CardGridComponent,
    EmptyStateComponent,
    EvalStatusBadgeComponent,
    LiveStatusComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    StatCardComponent,
    StatusIconComponent,
    WaitingNoticeComponent,
  ],
  templateUrl: './evaluation-summary.component.html',
  styleUrl: './evaluation-summary.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class EvaluationSummaryComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private evaluations = inject(EvaluationsService);
  private crumbs = inject(BreadcrumbsService);
  private live = inject(LiveService);
  private auth = inject(AuthService);
  private now = inject(CurrentTimeSignal).currentTimeMilliseconds;
  private refresh = new Subject<void>();
  private liveSub?: Subscription;
  protected readonly links = links;
  liveConnectionOpen = signal<boolean | null>(null);

  project = '';
  evaluationId = '';
  evaluation = signal<EvaluationResponse | null>(null);
  summary = signal<EvaluationFailureSummary | null>(null);
  totalBuildCount = signal<number | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);
  private taskWithUserPermissionsSignal = signal<TaskDetailsResponse | null>(null);

  waitingAccess = computed(() => {
    const task = this.taskWithUserPermissionsSignal();
    return waitingAccessFrom(task?.permissions ?? [], !!task && accessFromEntity(task).canTrigger, this.auth.user()?.superuser === true);
  });

  running = computed(() => {
    const evaluation = this.evaluation();
    return !!evaluation && isRunningEvaluationStatus(evaluation.status);
  });

  headline = computed(() => {
    const summary = this.summary();
    const evaluation = this.evaluation();
    return summary && evaluation ? headline(summary, evaluation.status) : 'Summary';
  });

  countTiles = computed(() => {
    const summary = this.summary();
    const evaluation = this.evaluation();
    return summary && evaluation ? getEvaluationSummaryCountTiles(summary, evaluation.status) : [];
  });

  breadcrumb = computed(() => {
    const task = this.evaluation()?.task_name;
    const page = { label: 'Summary' };

    const evaluation = this.evaluation()?.id;
    return task && evaluation
      ? this.crumbs.buildEvaluationPageBreadcrumbs(this.project, evaluation, task, page)
      : this.crumbs.project(this.project, page);
  });

  failureGroups = computed<FailureGroup[]>(() => {
    const summary = this.summary();
    if (!summary) return [];
    const groups = summary.compared_with
      ? [
          { title: 'Newly failing', failures: summary.failures.filter((f) => f.newly_failing) },
          { title: 'Still failing', failures: summary.failures.filter((f) => !f.newly_failing) },
        ]
      : [{ title: 'Failed builds', failures: summary.failures }];

    return groups.filter((group) => group.failures.length > 0);
  });

  newlyFailing = computed(() => {
    const summary = this.summary();
    if (!summary) return 0;

    return summary.failures.filter((f) => f.newly_failing).length
      + summary.failed_attributes.filter((f) => f.newly_failing).length;
  });

  duration = computed(() => {
    const evaluation = this.evaluation();
    return evaluation ? formatDuration(evaluationDuration(evaluation, this.now())) : '';
  });

  protected readonly commitLabel = commitLabel;
  protected readonly commitWebUrl = commitWebUrl;
  protected readonly repositoryWebUrl = repositoryWebUrl;
  protected readonly failureKind = failureKind;
  protected readonly failureTitle = failureTitle;
  protected readonly firstLine = firstLine;
  protected readonly blockedLine = blockedLine;
  protected readonly plural = plural;

  constructor() {
    this.refresh
      .pipe(auditTime(SUMMARY_REFRESH_MS), switchMap(() => this.fetch().pipe(catchError(() => EMPTY))), takeUntilDestroyed())
      .subscribe();
    inject(DestroyRef).onDestroy(() => this.stopFollowing());
  }

  ngOnInit(): void {
    this.project = this.route.snapshot.paramMap.get('project') ?? '';
    this.evaluationId = this.route.snapshot.paramMap.get('evaluationId') ?? '';
    const resolved = this.route.snapshot.data?.['evaluationAccess'] as EvaluationWithOptionalTask | undefined;
    this.taskWithUserPermissionsSignal.set(resolved?.task ?? null);
    this.load(resolved?.evaluation);
  }

  load(resolvedEvaluation?: EvaluationResponse): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.fetch(resolvedEvaluation).subscribe({
      next: () => {
        this.loading.set(false);
        this.followWhileRunning();
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private fetch(resolvedEvaluation?: EvaluationResponse): Observable<unknown> {
    return forkJoin({
      evaluation: resolvedEvaluation ? of(resolvedEvaluation) : this.evaluations.getEvaluation(this.evaluationId),
      summary: this.evaluations.getEvaluationSummary(this.evaluationId),
      builds: this.evaluations.getBuilds(this.evaluationId, 1).pipe(catchError(() => of(null))),
    }).pipe(
      tap(({ evaluation, summary, builds }) => {
        this.rememberTaskName(evaluation);
        this.evaluation.set(evaluation);
        this.summary.set(summary);
        this.totalBuildCount.set(builds?.total ?? null);
        if (!this.running()) this.stopFollowing();
      }),
    );
  }

  private followWhileRunning(): void {
    if (!this.running() || this.liveSub) return;
    this.liveSub = this.live
      .connect<LiveEvent>(
        `/evals/${this.evaluationId}/live`,
        createResyncOnLiveSocketReconnectCallback(() => this.refresh.next(), (open) => this.liveConnectionOpen.set(open)),
      )
      .pipe(filter((event) => event.event !== 'evaluation.activity'))
      .subscribe(() => this.refresh.next());
  }

  private stopFollowing(): void {
    this.liveSub?.unsubscribe();
    this.liveSub = undefined;
    this.liveConnectionOpen.set(null);
  }

  private rememberTaskName(evaluation: EvaluationResponse): void {
    if (!evaluation.task_name || !evaluation.task_display_name) return;
    this.crumbs.rememberTask(this.project, evaluation.task_name, evaluation.task_display_name);
  }
}
