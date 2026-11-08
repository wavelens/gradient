/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Subscription, catchError, forkJoin, interval, map, startWith, switchMap, takeWhile } from 'rxjs';
import { inBackground } from '@core/interceptors/request-context';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  LoadingSpinnerComponent,
  MenuComponent,
  type MenuItem,
  MessageBannerComponent,
  DrawerDirective,
  SelectButtonComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent, StatusBadgeComponent, StatusIconComponent, WaitingNoticeComponent } from '@shared/ui';
import { AuthService } from '@core/services/auth.service';
import { JobTimelineComponent } from '@features/board/job-detail/job-timeline.component';
import { TerminalComponent, type TerminalSource, terminalSocket } from '@shared/terminal';
import { LogViewerComponent, splitLogLines } from '@shared/log';
import { WritableDirective, AccessService } from '@shared/access';
import { clockTime, formatDuration, monthDay, plural, serverTime } from '@shared/text';
import { evaluationPhase, waitingAccessFrom } from '@shared/evaluation';
import { injectTaskAccess, injectTaskAccessData } from '@core/resolvers/inject-access';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import type { ExecJobPullRequestView, ExecJobResponse } from '@core/models';
import { abortExecJobConfirm, deviceLabel, execJobMenu, guestSizeLabel, hasExecTerminal, isOpenExecJob, execJobPhase } from '../exec-job-status';
import { phaseMarker } from '../phase-markers';
import { ExecJobOutputsComponent } from '../exec-job-outputs/exec-job-outputs.component';
import { ExecJobInfoComponent } from '../exec-job-info/exec-job-info.component';

const POLL_MS = 2000;

type JobView = 'log' | 'info' | 'timeline' | 'terminal';
type TerminalMode = 'console' | 'attach' | 'shell';

const TERMINAL_PATHS: Record<TerminalMode, (node: number) => string> = {
  console: (node) => `console?node=${node}`,
  attach: (node) => `shell?node=${node}&attach=true`,
  shell: (node) => `shell?node=${node}`,
};

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
    RouterModule, FormsModule, BadgeComponent, ButtonComponent, CopyFieldComponent,
    EmptyStateComponent, FormFieldComponent, IconComponent, LoadingSpinnerComponent, MenuComponent, MessageBannerComponent,
    SelectButtonComponent, StatusBadgeComponent, StatusIconComponent, TerminalComponent, WritableDirective, ExecJobOutputsComponent,
    LogViewerComponent, JobTimelineComponent, LabelHelpComponent, ExecJobInfoComponent, WaitingNoticeComponent, DrawerDirective,
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
  private router = inject(Router);
  private destroyRef = inject(DestroyRef);
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
  private log = '';
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
  keptExitCode = computed(() => {
    const job = this.job();
    return this.open() && job?.keep_vm_running ? job.program_exit_code ?? null : null;
  });
  private taskData = injectTaskAccessData();
  waitingAccess = computed(() =>
    waitingAccessFrom(this.taskData()?.task.permissions ?? [], this.triggerAccess().canEdit, this.auth.user()?.superuser === true),
  );
  jobMenu = computed<MenuItem[]>(() => {
    const job = this.job();
    if (!job) return [];
    return execJobMenu(job, this.triggerAccess().canEdit, {
      terminal: { command: () => this.showView('terminal') },
      abort: () => this.abort(),
    });
  });
  crumbs = computed(() => {
    const job = this.job();
    return this.breadcrumbs.taskAt(this.projectName, this.taskName, job?.evaluation, { label: job?.entry_point ?? 'Exec job' }).slice(1);
  });
  duration = computed(() => {
    const job = this.job();
    if (!job?.started_at) return formatDuration(null);
    const end = job.finished_at ? serverTime(job.finished_at).getTime() : this.now();
    return formatDuration(Math.max(0, end - serverTime(job.started_at).getTime()));
  });

  protected readonly phase = execJobPhase;
  protected readonly links = links;
  protected readonly device = deviceLabel;
  protected readonly guestSize = guestSizeLabel;
  protected readonly time = clockTime;
  protected readonly plural = plural;
  protected readonly day = monthDay;
  protected readonly evaluationPhase = evaluationPhase;
  protected readonly phaseMarker = phaseMarker;

  ngOnInit(): void {
    const params = this.route.snapshot.paramMap;
    this.projectName = params.get('project') ?? '';
    this.taskName = params.get('task') ?? '';
    this.jobId = params.get('job') ?? '';
    this.wantsTerminal = this.route.snapshot.queryParamMap?.get('view') === 'terminal';
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
    this.poll = interval(POLL_MS).pipe(
      startWith(-1),
      switchMap((tick) => forkJoin({
        job: this.execJobs.get(this.jobId, tick >= 0 ? inBackground() : undefined),
        pullRequests: this.execJobs.pullRequests(this.jobId, inBackground()),
      }).pipe(
        switchMap(({ job, pullRequests }) => {
          if (this.node() !== 0 && !job.node_jobs.some((n) => n.node === this.node())) this.setSelectedNodeIndexAndUrl(0);
          return this.execJobs.log(this.jobId, undefined, this.node(), inBackground()).pipe(map((log) => ({ job, log, pullRequests })));
        }),
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
      if (this.terminalReachable()) {
        if (this.wantsTerminal) this.showView('terminal');
      } else if (this.view() === 'terminal') {
        this.showView('log');
      }
      this.wantsTerminal = false;
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
        this.notify.success('Exec job started');
        this.watch();
      },
      error: (error: unknown) => {
        this.starting.set(false);
        this.notify.failure('Failed to start exec job', error);
      },
    });
  }
}
