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
import type { RunnerJobSummary } from '@core/models';
import { RunnerJobsService } from '@core/services/runner-jobs.service';
import { deviceLabel, isOpenRunnerJob, runnerJobPhase, waitingReasonText } from '../runner-job-status';

const POLL_MS = 4000;

@Component({
  selector: 'app-runner-job-list',
  standalone: true,
  imports: [RouterModule, BadgeComponent, StatusIconComponent],
  templateUrl: './runner-job-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './runner-job-list.component.scss',
})
export class RunnerJobListComponent implements OnChanges {
  private runnerJobs = inject(RunnerJobsService);

  evaluation = input.required<string>();
  evaluationRunning = input(false);
  project = input.required<string>();
  task = input.required<string>();

  jobs = signal<RunnerJobSummary[]>([]);
  private poll?: Subscription;

  protected readonly phase = runnerJobPhase;
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
      switchMap(() => this.runnerJobs.forEvaluation(evaluation).pipe(catchError(() => of(null)))),
      takeWhile((jobs) => this.evaluationRunning() || jobs === null || jobs.some((job) => isOpenRunnerJob(job.status)), true),
    ).subscribe((jobs) => {
      if (jobs) this.jobs.set(jobs);
    });
  }

  detail(job: RunnerJobSummary): string {
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
