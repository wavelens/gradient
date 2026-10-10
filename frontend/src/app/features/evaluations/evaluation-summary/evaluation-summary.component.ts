/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { forkJoin } from 'rxjs';
import type { Evaluation } from '@core/models';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { EvaluationFailureSummary, EvaluationsService, FailedBuildSummary } from '@core/services/evaluations.service';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  EmptyStateComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  StatCardComponent,
} from '@gradient/ui/ui';
import {
  commitLabel,
  commitWebUrl,
  evaluationDuration,
  formatEvaluationDuration,
  isRunningEvaluationStatus,
  repositoryWebUrl,
} from '@shared/evaluation';
import { serverTime } from '@shared/text';
import { EvalStatusBadgeComponent, StatusIconComponent } from '@shared/ui';
import { blockedLine, failureKind, failureTitle, firstLine, headline } from './summary-text';

interface FailureGroup {
  title: string;
  failures: FailedBuildSummary[];
}

@Component({
  selector: 'app-evaluation-summary',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    BadgeComponent,
    ButtonComponent,
    CardGridComponent,
    EmptyStateComponent,
    EvalStatusBadgeComponent,
    LoadingSpinnerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    StatCardComponent,
    StatusIconComponent,
  ],
  templateUrl: './evaluation-summary.component.html',
  styleUrl: './evaluation-summary.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class EvaluationSummaryComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private evaluations = inject(EvaluationsService);
  private crumbs = inject(BreadcrumbsService);

  project = '';
  evaluationId = '';
  evaluation = signal<Evaluation | null>(null);
  summary = signal<EvaluationFailureSummary | null>(null);
  loading = signal(true);

  running = computed(() => {
    const evaluation = this.evaluation();
    return !!evaluation && isRunningEvaluationStatus(evaluation.status);
  });

  headline = computed(() => {
    const summary = this.summary();
    return summary ? headline(summary, this.running()) : 'Summary';
  });

  breadcrumb = computed(() => {
    const task = this.evaluation()?.task_name;
    const page = { label: 'Summary' };

    return task ? this.crumbs.task(this.project, task, page) : this.crumbs.project(this.project, page);
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
    return evaluation ? formatEvaluationDuration(evaluationDuration(evaluation, Date.now())) : '';
  });

  protected readonly commitLabel = commitLabel;
  protected readonly commitWebUrl = commitWebUrl;
  protected readonly repositoryWebUrl = repositoryWebUrl;
  protected readonly serverTime = serverTime;
  protected readonly failureKind = failureKind;
  protected readonly failureTitle = failureTitle;
  protected readonly firstLine = firstLine;

  ngOnInit(): void {
    this.project = this.route.snapshot.paramMap.get('project') ?? '';
    this.evaluationId = this.route.snapshot.paramMap.get('evaluationId') ?? '';
    forkJoin({
      evaluation: this.evaluations.getEvaluation(this.evaluationId),
      summary: this.evaluations.getEvaluationSummary(this.evaluationId),
    }).subscribe({
      next: ({ evaluation, summary }) => {
        this.rememberTaskName(evaluation);
        this.evaluation.set(evaluation);
        this.summary.set(summary);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  private rememberTaskName(evaluation: Evaluation): void {
    if (!evaluation.task_name || !evaluation.task_display_name) return;
    this.crumbs.rememberTask(this.project, evaluation.task_name, evaluation.task_display_name);
  }

  failureMeta(failure: FailedBuildSummary): string {
    const derivation = failure.attributes.length ? failure.name : '';

    return [derivation, blockedLine(failure)].filter((part) => part.length > 0).join(' \u00b7 ');
  }
}
