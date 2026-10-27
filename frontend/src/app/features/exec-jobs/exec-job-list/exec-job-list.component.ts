/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnChanges, SimpleChanges, inject, input, signal } from '@angular/core';
import { RouterModule } from '@angular/router';
import { Subscription, catchError, interval, of, startWith, switchMap, takeWhile } from 'rxjs';
import { BadgeComponent } from '@gradient/ui/ui';
import { StatusIconComponent } from '@shared/ui';
import type { ExecJobSummary } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { deviceLabel, isOpenExecJob, execJobPhase, waitingReasonText } from '../exec-job-status';

const POLL_MS = 4000;

@Component({
  selector: 'app-exec-job-list',
  standalone: true,
  imports: [RouterModule, BadgeComponent, StatusIconComponent],
  templateUrl: './exec-job-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './exec-job-list.component.scss',
})
export class ExecJobListComponent implements OnChanges {
  private execJobs = inject(ExecJobsService);

  evaluation = input.required<string>();
  evaluationRunning = input(false);
  project = input.required<string>();
  task = input.required<string>();

  jobs = signal<ExecJobSummary[]>([]);
  private poll?: Subscription;

  protected readonly phase = execJobPhase;
  protected readonly device = deviceLabel;

  constructor() {
    inject(DestroyRef).onDestroy(() => this.poll?.unsubscribe());
  }

  ngOnChanges(changes: SimpleChanges): void {
    if (changes['evaluation']) {
      this.jobs.set([]);
      this.watch();
    } else if (this.poll?.closed) {
      this.watch();
    }
  }

  private watch(): void {
    this.poll?.unsubscribe();
    const evaluation = this.evaluation();
    this.poll = interval(POLL_MS).pipe(
      startWith(0),
      switchMap(() => this.execJobs.forEvaluation(evaluation).pipe(
        catchError((error: unknown) => of(error instanceof ApiError && !error.unreachable ? 'refused' as const : null)),
      )),
      takeWhile((jobs) => jobs !== 'refused'
        && (this.evaluationRunning() || jobs === null || jobs.some((job) => isOpenExecJob(job.status))), true),
    ).subscribe((jobs) => {
      if (Array.isArray(jobs)) this.jobs.set(jobs);
    });
  }

  detail(job: ExecJobSummary): string {
    const reason = waitingReasonText(job.waiting_reason);
    switch (job.status) {
      case 'created': return 'waiting for its builds';
      case 'queued': return reason ?? 'queued';
      case 'starting': return 'starting';
      case 'running': return job.last_phase ? `running, phase ${job.last_phase}` : 'running';
      case 'completed': return job.last_phase ? `completed, phase ${job.last_phase}` : 'completed';
      case 'failed': return job.message ?? 'failed';
      case 'aborted': return 'aborted';
    }
  }
}
