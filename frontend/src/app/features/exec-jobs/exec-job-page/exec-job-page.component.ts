/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DOCUMENT, DestroyRef, OnInit, computed, effect, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute, NavigationEnd, Router, RouterModule } from '@angular/router';
import { Title } from '@angular/platform-browser';
import { FormsModule } from '@angular/forms';
import { takeUntilDestroyed, toSignal } from '@angular/core/rxjs-interop';
import { EMPTY, Subscription, catchError, filter, forkJoin, map, of, switchMap, takeWhile } from 'rxjs';
import {
  ButtonComponent,
  CopyFieldComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  DrawerDirective,
  SelectButtonComponent,
  TabSwitchComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent, StatusBadgeComponent, StatusIconComponent, WaitingNoticeComponent } from '@shared/ui';
import { AuthService } from '@core/services/auth.service';
import { JobTimelineComponent } from '@features/board/job-detail/job-timeline.component';
import { TerminalComponent, type TerminalSource, terminalSocket } from '@shared/terminal';
import { LogViewerComponent, appendLogChunkToSplitLines } from '@shared/log';
import { refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible } from '@shared/paging';
import { ApiError } from '@core/services/api.service';
import { WritableDirective, AccessService } from '@shared/access';
import { formatDuration, plural, RelativeTimePipe, serverTime, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { buildAttemptEndReasonLabel, evaluationPhase, execJobPhase, waitingAccessFrom } from '@shared/evaluation';
import { injectTaskAccess, injectTaskAccessData } from '@core/resolvers/inject-access';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { composeTitle } from '@core/title/gradient-title-strategy';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import type { ExecJobAttemptView, ExecJobLog, ExecJobPullRequestView, ExecJobResponse } from '@core/models';
import {
  abortExecJobConfirm, getExecJobStatusBadgeLabel, hasExecTerminal, isExecJobWaitingToStart, isOpenExecJob,
} from '../exec-job-status';
import { phaseMarker } from '../phase-markers';
import { ExecJobOutputsComponent } from '../exec-job-outputs/exec-job-outputs.component';
import { ExecJobInfoComponent } from '../exec-job-info/exec-job-info.component';

const POLL_MS = 2000;
const execJobPollingStopOnServerRefusal = Symbol('refused');

type JobView = 'log' | 'info' | 'timeline' | 'terminal';
type TerminalMode = 'console' | 'attach' | 'shell';

const TERMINAL_PATHS: Record<TerminalMode, (node: number) => string> = {
  console: (node) => `console?node=${node}`,
  attach: (node) => `shell?node=${node}&attach=true`,
  shell: (node) => `shell?node=${node}`,
};

function latestAttemptNumber(job: ExecJobResponse, node: number): number | undefined {
  const attempts = node === 0 ? job.attempts : job.node_jobs.find((n) => n.node === node)?.attempts ?? [];
  return attempts.length === 0 ? undefined : Math.max(...attempts.map((a) => a.number));
}

function nodeFromQuery(value: string | null | undefined): number {
  const node = Number(value);
  return Number.isInteger(node) && node > 0 ? node : 0;
}

const LOG_VIEW = { label: 'Log', value: 'log', icon: 'article' } as const;
const INFO_VIEW = { label: 'Info', value: 'info', icon: 'info' } as const;
const TIMELINE_VIEW = { label: 'Timeline', value: 'timeline', icon: 'schedule' } as const;
const TERMINAL_VIEW = { label: 'Terminal', value: 'terminal', icon: 'terminal' } as const;

@Component({
  selector: 'app-exec-job-page',
  standalone: true,
  imports: [
    RouterModule, FormsModule, ButtonComponent, CopyFieldComponent,
    EmptyStateComponent, FormFieldComponent, IconComponent, LoadingSpinnerComponent, MessageBannerComponent,
    SelectButtonComponent, TabSwitchComponent, StatusBadgeComponent, StatusIconComponent, TerminalComponent, WritableDirective, ExecJobOutputsComponent,
    LogViewerComponent, JobTimelineComponent, LabelHelpComponent, ExecJobInfoComponent, WaitingNoticeComponent, DrawerDirective, RelativeTimePipe,
    ServerTimestampToLocalDateTimePipe,
  ],
  templateUrl: './exec-job-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: [
    '../../../shared/log/log-page.scss',
    '../../../shared/log/log-sidebar.scss',
    '../../../shared/evaluation/phone-breadcrumb.scss',
    './exec-job-page.component.scss',
  ],
})
export class ExecJobPageComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private destroyRef = inject(DestroyRef);
  private document = inject(DOCUMENT);
  private execJobs = inject(ExecJobsService);
  private breadcrumbs = inject(BreadcrumbsService);
  private auth = inject(AuthService);
  private accessService = inject(AccessService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private viewer = viewChild(LogViewerComponent);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  projectName = '';
  taskName = '';
  private jobId = '';
  private poll?: Subscription;
  private logFinishedLines: string[] = [];
  private logUnfinishedLine = '';
  private logSource: string | null = null;
  private logBytes = 0;
  private now = signal(Date.now());
  private wantsTerminal = false;

  job = signal<ExecJobResponse | null>(null);
  node = signal(0);
  logLines = signal<readonly string[]>([]);
  pullRequests = signal<ExecJobPullRequestView[]>([]);
  starting = signal(false);
  switchingKeep = signal(false);
  errorMessage = signal<string | null>(null);
  sidebarOpen = signal(false);

  view = signal<JobView>('log');
  terminalMode = signal<TerminalMode>('console');
  terminal = signal<TerminalSource | null>(null);

  terminalReachable = computed(() => {
    const job = this.job();
    return !!job && this.triggerAccess().canEdit && hasExecTerminal(job.status);
  });
  views = computed(() => [LOG_VIEW, INFO_VIEW, TIMELINE_VIEW, ...(this.terminalReachable() ? [TERMINAL_VIEW] : [])]);
  terminalModes = computed(() => {
    const job = this.job();
    const modes: { label: string; value: TerminalMode }[] = [{ label: 'Console', value: 'console' }];
    if (job?.status === 'running' && job.ssh_command) {
      if (job.program_exit_code == null) modes.push({ label: 'Attach to program', value: 'attach' });
      modes.push({ label: 'New shell', value: 'shell' });
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
  keepVmRunning = computed(() => this.job()?.keep_vm_running ?? false);
  keepVmRunningToggleIsApplicable = computed(() => {
    const job = this.job();
    return !!job && (hasExecTerminal(job.status) || (this.open() && job.keep_vm_running));
  });
  keptExitCode = computed(() => {
    const job = this.job();
    return this.open() && job?.keep_vm_running ? job.program_exit_code ?? null : null;
  });
  private taskData = injectTaskAccessData();
  waitingAccess = computed(() =>
    waitingAccessFrom(this.taskData()?.task.permissions ?? [], this.triggerAccess().canEdit, this.auth.user()?.superuser === true),
  );
  jobBoardEntryRouterLink = computed(() => {
    const job = this.job();
    return job && this.auth.isAuthenticated() ? links.boardJobOrCluster(job.dispatched_job, job.cluster_job) : undefined;
  });
  execJobAndViewLabelTitle = computed(() => {
    const job = this.job();
    const viewLabel = this.views().find((option) => option.value === this.view())?.label;
    return job ? `${job.entry_point} · ${viewLabel}` : '';
  });
  private routerNavigationFinished = toSignal(this.router.events.pipe(filter((event) => event instanceof NavigationEnd)));
  crumbs = computed(() => {
    const job = this.job();
    return this.breadcrumbs.taskAt(this.projectName, this.taskName, job?.evaluation, { label: job?.entry_point ?? 'Exec job' });
  });
  projectAndTaskCrumbs = computed(() => this.crumbs().slice(-3, -1));
  highestNumberedAttempt = computed(() => {
    const job = this.job();
    if (!job || isExecJobWaitingToStart(job.status)) return null;
    return job.attempts.reduce<ExecJobAttemptView | null>((latest, a) => (!latest || a.number > latest.number ? a : latest), null);
  });
  firstAttemptStartedAt = computed(() => {
    const job = this.job();
    if (!job || isExecJobWaitingToStart(job.status)) return null;
    return job.attempts.reduce<ExecJobAttemptView | null>((first, a) => (!first || a.number < first.number ? a : first), null)?.started_at ?? null;
  });
  statusBadgeLabel = computed(() => {
    const job = this.job();
    if (!job) return '';
    const endReason = this.highestNumberedAttempt()?.end_reason;
    return job.status === 'failed' && endReason ? buildAttemptEndReasonLabel(endReason) : getExecJobStatusBadgeLabel(job);
  });
  formattedProgramRunTime = computed(() => {
    const attempt = this.highestNumberedAttempt();
    if (!attempt?.program_started_at) return '';
    if (!attempt.ended_at) return formatDuration(Math.max(0, this.now() - serverTime(attempt.program_started_at).getTime()));
    const programPhase = this.job()?.timeline.filter((phase) => phase.phase === 'program').at(-1);
    return formatDuration(programPhase
      ? programPhase.end_ms - programPhase.start_ms
      : serverTime(attempt.ended_at).getTime() - serverTime(attempt.program_started_at).getTime());
  });

  protected readonly phase = execJobPhase;
  protected readonly links = links;
  protected readonly plural = plural;
  protected readonly evaluationPhase = evaluationPhase;
  protected readonly phaseMarker = phaseMarker;

  constructor() {
    const title = inject(Title);
    effect(() => {
      this.routerNavigationFinished();
      const pageTitle = this.execJobAndViewLabelTitle();
      if (pageTitle) title.setTitle(composeTitle(pageTitle, undefined));
    });
  }

  ngOnInit(): void {
    const params = this.route.snapshot.paramMap;
    this.projectName = params.get('project') ?? '';
    this.taskName = params.get('task') ?? '';
    this.jobId = params.get('job') ?? '';
    const view = this.route.snapshot.queryParamMap?.get('view');
    this.wantsTerminal = view === 'terminal';
    if (view === INFO_VIEW.value || view === TIMELINE_VIEW.value) this.view.set(view);
    this.node.set(nodeFromQuery(this.route.snapshot.queryParamMap?.get('node')));
    this.watch();
  }

  pickNode(node: number): void {
    this.setSelectedNodeIndexAndUrl(node);
    this.sidebarOpen.set(false);
    this.watch();
    if (this.view() === 'terminal') this.openTerminal(this.terminalMode());
  }

  private setSelectedNodeIndexAndUrl(node: number): void {
    this.node.set(node);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { node: node === 0 ? null : node },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  private watch(): void {
    this.poll?.unsubscribe();
    this.poll = refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible(this.document, POLL_MS).pipe(
      switchMap(() => forkJoin({
        job: this.execJobs.get(this.jobId),
        pullRequests: this.execJobs.pullRequests(this.jobId),
      }).pipe(
        switchMap(({ job, pullRequests }) => {
          if (this.node() !== 0 && !job.node_jobs.some((n) => n.node === this.node())) this.setSelectedNodeIndexAndUrl(0);
          if (isExecJobWaitingToStart(job.status)) return of({ job, pullRequests, log: null });
          const node = this.node();
          const attempt = latestAttemptNumber(job, node);
          const source = `${node}:${attempt ?? ''}`;
          const offset = source === this.logSource ? this.logBytes : undefined;
          return this.execJobs.logAfter(this.jobId, offset, attempt, node).pipe(map((chunk) => ({ job, pullRequests, log: { source, chunk } })));
        }),
        catchError((error: Error) => {
          this.errorMessage.set(error.message);
          return error instanceof ApiError && !error.unreachable ? of(execJobPollingStopOnServerRefusal) : EMPTY;
        }),
      )),
      takeWhile((update) => update !== execJobPollingStopOnServerRefusal && isOpenExecJob(update.job.status), true),
      takeUntilDestroyed(this.destroyRef),
    ).subscribe((update) => {
      if (update === execJobPollingStopOnServerRefusal) return;
      const { job, log, pullRequests } = update;
      this.errorMessage.set(null);
      this.now.set(Date.now());
      this.job.set(job);
      this.showLog(log);
      this.pullRequests.set(pullRequests);
      if (this.terminalReachable()) {
        if (this.wantsTerminal) this.showView('terminal');
      } else if (this.view() === 'terminal') {
        this.showView('log');
      }
      this.wantsTerminal = false;
    });
  }

  private showLog(update: { source: string; chunk: ExecJobLog } | null): void {
    const continues = update !== null && update.chunk.offset !== 0;
    this.logSource = update?.source ?? null;
    this.logBytes = update?.chunk.total_bytes ?? 0;
    if (!update?.chunk.log && (continues || this.logLines().length === 0)) return;
    const appended = appendLogChunkToSplitLines(
      continues ? this.logFinishedLines : [],
      continues ? this.logUnfinishedLine : '',
      update?.chunk.log ?? '',
    );
    this.logFinishedLines = appended.finished;
    this.logUnfinishedLine = appended.unfinished;
    this.logLines.set(appended.lines);
  }

  showView(view: JobView): void {
    this.view.set(view);
    this.sidebarOpen.set(false);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { view: view === LOG_VIEW.value ? null : view },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
    if (view === 'terminal') this.openTerminal(this.terminalMode());
    else this.terminal.set(null);
  }

  showPhase(line: number): void {
    this.showView('log');
    setTimeout(() => void this.viewer()?.jumpToLine(line), 0);
  }

  openTerminal(mode: TerminalMode): void {
    this.terminalMode.set(mode);
    this.terminal.set(terminalSocket(`/exec/${this.jobId}/${TERMINAL_PATHS[mode](this.node())}`));
  }

  abort(): void {
    const target = this.job()?.entry_point ?? this.jobId;
    this.confirm.confirm(abortExecJobConfirm(target, () => this.execJobs.abort(this.jobId))).subscribe(() => {
      this.notify.success('Exec job aborted');
      this.watch();
    });
  }

  toggleKeepVmRunning(): void {
    this.switchKeepVmRunning(!this.keepVmRunning());
  }

  letVmStop(): void {
    this.switchKeepVmRunning(false);
  }

  private switchKeepVmRunning(keep: boolean): void {
    this.switchingKeep.set(true);
    const request = keep ? this.execJobs.keepVmRunning(this.jobId) : this.execJobs.letVmStop(this.jobId);
    request.subscribe({
      next: () => {
        this.switchingKeep.set(false);
        this.notify.success(keep ? 'VM running after program exit' : 'VM shutdown at program end');
        this.watch();
      },
      error: (error: unknown) => {
        this.switchingKeep.set(false);
        this.notify.failure(keep ? 'Failed to keep VM running' : 'Failed to let VM stop', error);
      },
    });
  }

  run(): void {
    this.starting.set(true);
    this.execJobs.run(this.jobId).subscribe({
      next: () => {
        this.starting.set(false);
        this.notify.success('Exec job added to queue');
        this.watch();
      },
      error: (error: unknown) => {
        this.starting.set(false);
        this.notify.failure('Failed to start exec job', error);
      },
    });
  }
}
