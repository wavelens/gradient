/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Subscription, catchError, forkJoin, interval, startWith, switchMap, takeWhile } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  SelectButtonComponent,
  SelectComponent,
  TabSwitchComponent,
  TableComponent,
} from '@gradient/ui/ui';
import { StatusIconComponent } from '@shared/ui';
import { TerminalComponent, type TerminalSource, terminalSocket } from '@shared/terminal';
import { WritableDirective, AccessService } from '@shared/access';
import { clockTime, monthDay } from '@shared/text';
import { evaluationPhase } from '@shared/evaluation';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import type { ExecJobPullRequestView, ExecJobResponse, ExecJobStatus } from '@core/models';
import { deviceLabel, isOpenExecJob, execJobPhase, waitingReasonText } from '../exec-job-status';
import { splitPhaseMarkers } from '../phase-markers';
import { ExecJobOutputsComponent } from '../exec-job-outputs/exec-job-outputs.component';

const POLL_MS = 2000;

type JobView = 'log' | 'terminal';
type TerminalMode = 'console' | 'attach' | 'shell';

const TERMINAL_PATHS: Record<TerminalMode, (node: number) => string> = {
  console: (node) => `console?node=${node}`,
  attach: (node) => `shell?node=${node}&attach=true`,
  shell: (node) => `shell?node=${node}`,
};

const nodeLabel = (node: number): string => (node === 0 ? 'Node 0 (master)' : `Node ${node}`);

function nodeFromQuery(value: string | null): number {
  const node = Number(value);
  return Number.isInteger(node) && node > 0 ? node : 0;
}

const hasTerminal = (status: ExecJobStatus): boolean => status === 'starting' || status === 'running';

@Component({
  selector: 'app-exec-job-page',
  standalone: true,
  imports: [
    RouterModule, FormsModule, BadgeComponent, ButtonComponent, CopyFieldComponent, DialogComponent,
    FormFieldComponent, LoadingSpinnerComponent, PageLayoutComponent, SelectButtonComponent, SelectComponent,
    TabSwitchComponent, TableComponent, StatusIconComponent, TerminalComponent, WritableDirective,
    ExecJobOutputsComponent,
  ],
  templateUrl: './exec-job-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './exec-job-page.component.scss',
})
export class ExecJobPageComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private destroyRef = inject(DestroyRef);
  private execJobs = inject(ExecJobsService);
  private crumbs = inject(BreadcrumbsService);
  private accessService = inject(AccessService);

  access = injectTaskAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  projectName = '';
  taskName = '';
  private jobId = '';
  private poll?: Subscription;

  job = signal<ExecJobResponse | null>(null);
  node = signal(0);
  nodeOptions = computed(() =>
    Array.from({ length: this.job()?.nodes ?? 1 }, (_, node) => ({ label: nodeLabel(node), value: node })),
  );
  log = signal('');
  logSegments = computed(() => splitPhaseMarkers(this.log()));
  pullRequests = signal<ExecJobPullRequestView[]>([]);
  abortOpen = signal(false);
  aborting = signal(false);
  errorMessage = signal<string | null>(null);

  protected readonly views = [
    { label: 'Log', value: 'log' },
    { label: 'Terminal', value: 'terminal' },
  ];
  view = signal<JobView>('log');
  terminalMode = signal<TerminalMode>('console');
  terminal = signal<TerminalSource | null>(null);

  terminalReachable = computed(() => {
    const job = this.job();
    return job ? hasTerminal(job.status) : false;
  });
  terminalModes = computed(() => {
    const job = this.job();
    const modes: { label: string; value: TerminalMode }[] = [{ label: 'Console', value: 'console' }];
    if (job?.status === 'running' && job.ssh_command) {
      modes.push({ label: 'Attach to program', value: 'attach' }, { label: 'New shell', value: 'shell' });
    }
    return modes;
  });

  open = computed(() => {
    const job = this.job();
    return job ? isOpenExecJob(job.status) : false;
  });
  waiting = computed(() => waitingReasonText(this.job()?.waiting_reason));
  breadcrumb = computed(() => this.crumbs.task(this.projectName, this.taskName, { label: this.job()?.entry_point ?? 'Exec job' }));

  protected readonly phase = execJobPhase;
  protected readonly device = deviceLabel;
  protected readonly time = clockTime;
  protected readonly day = monthDay;
  protected readonly evaluationPhase = evaluationPhase;

  ngOnInit(): void {
    const params = this.route.snapshot.paramMap;
    this.projectName = params.get('project') ?? '';
    this.taskName = params.get('task') ?? '';
    this.jobId = params.get('job') ?? '';
    this.node.set(nodeFromQuery(this.route.snapshot.queryParamMap.get('node')));
    this.watch();
  }

  pickNode(node: number): void {
    this.node.set(node);
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { node: node === 0 ? null : node },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
    this.watch();
    if (this.view() === 'terminal') this.openTerminal(this.terminalMode());
  }

  private watch(): void {
    this.poll?.unsubscribe();
    this.poll = interval(POLL_MS).pipe(
      startWith(0),
      switchMap(() => forkJoin({
        job: this.execJobs.get(this.jobId),
        log: this.execJobs.log(this.jobId, undefined, this.node()),
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
      this.job.set(job);
      this.log.set(log);
      this.pullRequests.set(pullRequests);
      if (!hasTerminal(job.status)) this.showView('log');
    });
  }

  showView(view: JobView): void {
    this.view.set(view);
    if (view === 'terminal') this.openTerminal(this.terminalMode());
    else this.terminal.set(null);
  }

  openTerminal(mode: TerminalMode): void {
    this.terminalMode.set(mode);
    this.terminal.set(terminalSocket(`/exec-jobs/${this.jobId}/${TERMINAL_PATHS[mode](this.node())}`));
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
