/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnChanges, SimpleChanges, booleanAttribute, computed, inject, input, output, signal } from '@angular/core';
import { RouterModule, type UrlTree } from '@angular/router';
import { Subscription, catchError, interval, of, startWith, switchMap, takeWhile } from 'rxjs';
import { BadgeComponent, IconComponent, MenuComponent, type MenuItem } from '@gradient/ui/ui';
import { StatusIconComponent } from '@shared/ui';
import { execJobPhase } from '@shared/evaluation';
import type { ExecJobSummary } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import { abortExecJobConfirm, deviceLabel, execJobMenu, getExecJobStatusBadgeLabel, isOpenExecJob, waitingReasonText } from '../exec-job-status';

const POLL_MS = 4000;

@Component({
  selector: 'app-exec-job-list',
  standalone: true,
  imports: [RouterModule, BadgeComponent, IconComponent, MenuComponent, StatusIconComponent],
  templateUrl: './exec-job-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: ['../../../shared/evaluation/entry-rows.scss', '../../../shared/log/log-sidebar.scss', './exec-job-list.component.scss'],
})
export class ExecJobListComponent implements OnChanges {
  private execJobs = inject(ExecJobsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  evaluation = input.required<string>();
  evaluationRunning = input(false);
  project = input.required<string>();
  task = input.required<string>();
  canTrigger = input(false);
  isCompact = input(false, { transform: booleanAttribute });
  openExecJobsWaitingWithReason = output<ExecJobSummary[]>();
  allExecJobsLoadedOrPolled = output<ExecJobSummary[]>();

  jobs = signal<ExecJobSummary[]>([]);
  menuModel = signal<MenuItem[]>([]);
  execJobActionMenuItemsById = computed(() => new Map(this.jobs().map((job) => [job.id, this.jobMenu(job)])));
  private poll?: Subscription;

  protected readonly phase = execJobPhase;
  protected readonly statusLabel = getExecJobStatusBadgeLabel;
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
      if (!Array.isArray(jobs)) return;
      this.jobs.set(jobs);
      this.allExecJobsLoadedOrPolled.emit(jobs);
      this.openExecJobsWaitingWithReason.emit(jobs.filter((job) => isOpenExecJob(job.status) && !!job.waiting_reason));
    });
  }

  link(job: ExecJobSummary, terminal = false): UrlTree {
    return links.execJob(this.project(), this.task(), job.id, { terminal });
  }

  openMenu(event: Event, job: ExecJobSummary, menu: { toggle: (e: Event) => void }): void {
    event.stopPropagation();
    this.menuModel.set(this.execJobActionMenuItemsById().get(job.id) ?? []);
    menu.toggle(event);
  }

  openContextMenu(event: MouseEvent, job: ExecJobSummary, menu: { openAt: (e: MouseEvent) => void }): void {
    const items = this.execJobActionMenuItemsById().get(job.id) ?? [];
    if (items.length === 0) return;
    this.menuModel.set(items);
    menu.openAt(event);
  }

  private jobMenu(job: ExecJobSummary): MenuItem[] {
    return execJobMenu(job, this.canTrigger(), {
      terminal: { routerLink: this.link(job, true) },
      abort: () => this.abort(job),
    });
  }

  private abort(job: ExecJobSummary): void {
    this.confirm.confirm(abortExecJobConfirm(job.entry_point, () => this.execJobs.abort(job.id))).subscribe(() => {
      this.notify.success('Exec job aborted');
      this.watch();
    });
  }

  detail(job: ExecJobSummary): string {
    const reason = waitingReasonText(job.waiting_reason);
    switch (job.status) {
      case 'created': return reason ?? 'waiting for its builds';
      case 'queued': return reason ?? 'queued';
      case 'starting': return 'starting';
      case 'running': return job.last_phase ? `running, phase ${job.last_phase}` : 'running';
      case 'completed': return job.last_phase ? `completed, phase ${job.last_phase}` : 'completed';
      case 'failed': return job.message ?? 'failed';
      case 'aborted': return 'aborted';
    }
  }
}
