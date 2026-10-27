/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Subscription, catchError, forkJoin, interval, startWith, switchMap, takeWhile } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  TableComponent,
} from '@gradient/ui/ui';
import { StatusIconComponent } from '@shared/ui';
import { WritableDirective, AccessService } from '@shared/access';
import { clockTime } from '@shared/text';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { RunnerJobsService } from '@core/services/runner-jobs.service';
import type { RunnerJobResponse } from '@core/models';
import { deviceLabel, isOpenRunnerJob, runnerJobPhase, waitingReasonText } from '../runner-job-status';

const POLL_MS = 2000;

@Component({
  selector: 'app-runner-job-page',
  standalone: true,
  imports: [
    RouterModule, BadgeComponent, ButtonComponent, DialogComponent, LoadingSpinnerComponent,
    PageLayoutComponent, TableComponent, StatusIconComponent, WritableDirective,
  ],
  templateUrl: './runner-job-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './runner-job-page.component.scss',
})
export class RunnerJobPageComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private destroyRef = inject(DestroyRef);
  private runnerJobs = inject(RunnerJobsService);
  private crumbs = inject(BreadcrumbsService);
  private accessService = inject(AccessService);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  projectName = '';
  taskName = '';
  private jobId = '';
  private poll?: Subscription;

  job = signal<RunnerJobResponse | null>(null);
  log = signal('');
  abortOpen = signal(false);
  aborting = signal(false);
  errorMessage = signal<string | null>(null);

  open = computed(() => {
    const job = this.job();
    return job ? isOpenRunnerJob(job.status) : false;
  });
  waiting = computed(() => waitingReasonText(this.job()?.waiting_reason));
  breadcrumb = computed(() => this.crumbs.task(this.projectName, this.taskName, { label: this.job()?.entry_point ?? 'Runner job' }));

  protected readonly phase = runnerJobPhase;
  protected readonly device = deviceLabel;
  protected readonly time = clockTime;

  ngOnInit(): void {
    const params = this.route.snapshot.paramMap;
    this.projectName = params.get('project') ?? '';
    this.taskName = params.get('task') ?? '';
    this.jobId = params.get('job') ?? '';
    this.watch();
  }

  private watch(): void {
    this.poll?.unsubscribe();
    this.poll = interval(POLL_MS).pipe(
      startWith(0),
      switchMap(() => forkJoin({ job: this.runnerJobs.get(this.jobId), log: this.runnerJobs.log(this.jobId) }).pipe(
        catchError((error: Error) => {
          this.errorMessage.set(error.message);
          return EMPTY;
        }),
      )),
      takeWhile(({ job }) => isOpenRunnerJob(job.status), true),
      takeUntilDestroyed(this.destroyRef),
    ).subscribe(({ job, log }) => {
      this.errorMessage.set(null);
      this.job.set(job);
      this.log.set(log);
    });
  }

  confirmAbort(): void {
    this.aborting.set(true);
    this.runnerJobs.abort(this.jobId).subscribe({
      next: () => {
        this.aborting.set(false);
        this.abortOpen.set(false);
        this.watch();
      },
      error: (error: Error) => {
        this.aborting.set(false);
        this.abortOpen.set(false);
        this.errorMessage.set(error.message);
      },
    });
  }
}
