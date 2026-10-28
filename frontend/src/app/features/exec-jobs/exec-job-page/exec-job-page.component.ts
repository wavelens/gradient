/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Subscription, catchError, forkJoin, interval, startWith, switchMap, takeWhile } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  SelectButtonComponent,
} from '@gradient/ui/ui';
import { StatusBadgeComponent, StatusIconComponent } from '@shared/ui';
import { TerminalComponent, type TerminalSource, terminalSocket } from '@shared/terminal';
import { LogViewerComponent, parseLineFragment, splitLogLines } from '@shared/log';
import { WritableDirective, AccessService } from '@shared/access';
import { clockTime, monthDay } from '@shared/text';
import { evaluationPhase, formatEvaluationDuration, parseUtcTimestamp } from '@shared/evaluation';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import type { ExecJobPullRequestView, ExecJobResponse, ExecJobStatus } from '@core/models';
import { deviceLabel, isOpenExecJob, execJobPhase, waitingReasonText } from '../exec-job-status';
import { phaseMarker } from '../phase-markers';
import { ExecJobOutputsComponent } from '../exec-job-outputs/exec-job-outputs.component';

const POLL_MS = 2000;

type JobView = 'log' | 'terminal';
type TerminalMode = 'console' | 'attach' | 'shell';

const TERMINAL_PATHS: Record<TerminalMode, string> = {
  console: 'console',
  attach: 'shell?attach=true',
  shell: 'shell',
};

const LOG_VIEW = { label: 'Log', value: 'log', icon: 'article' } as const;
const TERMINAL_VIEW = { label: 'Terminal', value: 'terminal', icon: 'terminal' } as const;

const hasTerminal = (status: ExecJobStatus): boolean => status === 'starting' || status === 'running';

@Component({
  selector: 'app-exec-job-page',
  standalone: true,
  imports: [
    RouterModule, FormsModule, BadgeComponent, ButtonComponent, CopyFieldComponent, DialogComponent,
    EmptyStateComponent, FormFieldComponent, IconComponent, LoadingSpinnerComponent, MessageBannerComponent,
    SelectButtonComponent, StatusBadgeComponent, StatusIconComponent, TerminalComponent, WritableDirective, ExecJobOutputsComponent,
    LogViewerComponent,
  ],
  templateUrl: './exec-job-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: [
    '../../../shared/log/log-page.scss',
    '../../../shared/log/log-sidebar.scss',
    './exec-job-page.component.scss',
  ],
})
export class ExecJobPageComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private destroyRef = inject(DestroyRef);
  private execJobs = inject(ExecJobsService);
  private breadcrumbs = inject(BreadcrumbsService);
  private accessService = inject(AccessService);
  private viewer = viewChild(LogViewerComponent);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  projectName = '';
  taskName = '';
  private jobId = '';
  private poll?: Subscription;
  private log = '';
  private now = signal(Date.now());

  job = signal<ExecJobResponse | null>(null);
  logLines = signal<readonly string[]>([]);
  initialLine: number | null = null;
  pullRequests = signal<ExecJobPullRequestView[]>([]);
  abortOpen = signal(false);
  aborting = signal(false);
  errorMessage = signal<string | null>(null);
  sidebarOpen = signal(false);

  view = signal<JobView>('log');
  terminalMode = signal<TerminalMode>('console');
  terminal = signal<TerminalSource | null>(null);

  terminalReachable = computed(() => {
    const job = this.job();
    return job ? hasTerminal(job.status) : false;
  });
  views = computed(() => this.terminalReachable() ? [LOG_VIEW, TERMINAL_VIEW] : [LOG_VIEW]);
  terminalModes = computed(() => {
    const job = this.job();
    const modes: { label: string; value: TerminalMode }[] = [{ label: 'Console', value: 'console' }];
    if (job?.status === 'running' && job.ssh_command) {
      modes.push({ label: 'Attach to program', value: 'attach' }, { label: 'New shell', value: 'shell' });
    }
    return modes;
  });
  phaseLines = computed(() => this.logLines().flatMap((line, i) => {
    const name = phaseMarker(line);
    return name === null ? [] : [{ name, line: i + 1 }];
  }));

  open = computed(() => {
    const job = this.job();
    return job ? isOpenExecJob(job.status) : false;
  });
  waiting = computed(() => waitingReasonText(this.job()?.waiting_reason));
  crumbs = computed(() =>
    this.breadcrumbs.task(this.projectName, this.taskName, { label: this.job()?.entry_point ?? 'Exec job' }).slice(1),
  );
  duration = computed(() => {
    const job = this.job();
    if (!job) return '';
    const end = job.finished_at ? parseUtcTimestamp(job.finished_at) : this.now();
    return formatEvaluationDuration(end - parseUtcTimestamp(job.started_at ?? job.created_at));
  });

  protected readonly phase = execJobPhase;
  protected readonly device = deviceLabel;
  protected readonly time = clockTime;
  protected readonly day = monthDay;
  protected readonly evaluationPhase = evaluationPhase;
  protected readonly phaseMarker = phaseMarker;

  ngOnInit(): void {
    const params = this.route.snapshot.paramMap;
    this.projectName = params.get('project') ?? '';
    this.taskName = params.get('task') ?? '';
    this.jobId = params.get('job') ?? '';
    this.initialLine = parseLineFragment(this.route.snapshot.fragment);
    this.watch();
  }

  private watch(): void {
    this.poll?.unsubscribe();
    this.poll = interval(POLL_MS).pipe(
      startWith(0),
      switchMap(() => forkJoin({
        job: this.execJobs.get(this.jobId),
        log: this.execJobs.log(this.jobId),
        pullRequests: this.execJobs.pullRequests(this.jobId),
      }).pipe(
        catchError((error: Error) => {
          this.errorMessage.set(error.message);
          return EMPTY;
        }),
      )),
      takeWhile(({ job }) => isOpenExecJob(job.status), true),
      takeUntilDestroyed(this.destroyRef),
    ).subscribe(({ job, log, pullRequests }) => {
      this.errorMessage.set(null);
      this.now.set(Date.now());
      this.job.set(job);
      this.showLog(log);
      this.pullRequests.set(pullRequests);
      if (!hasTerminal(job.status)) this.showView('log');
    });
  }

  private showLog(log: string): void {
    if (log === this.log) return;
    this.log = log;
    this.logLines.set(splitLogLines(log));
  }

  showView(view: JobView): void {
    this.view.set(view);
    this.sidebarOpen.set(false);
    if (view === 'terminal') this.openTerminal(this.terminalMode());
    else this.terminal.set(null);
  }

  showPhase(line: number): void {
    this.showView('log');
    setTimeout(() => void this.viewer()?.jumpToLine(line), 0);
  }

  openTerminal(mode: TerminalMode): void {
    this.terminalMode.set(mode);
    this.terminal.set(terminalSocket(`/exec-jobs/${this.jobId}/${TERMINAL_PATHS[mode]}`));
  }

  confirmAbort(): void {
    this.aborting.set(true);
    this.execJobs.abort(this.jobId).subscribe({
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
