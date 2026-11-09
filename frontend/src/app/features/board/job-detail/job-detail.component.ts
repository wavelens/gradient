/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, DestroyRef, inject, signal, computed, effect, ChangeDetectionStrategy } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { CommonModule, Location } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { BoardService, AssignedJobDetail, InstanceContextView, Windowed } from '@core/services/board.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { AuthService } from '@core/services/auth.service';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  IconComponent,
  LoadingSpinnerComponent,
  LoadStateComponent,
  PopoverComponent,
  TableComponent,
} from '@gradient/ui/ui';
import {
  formatBytes,
  formatCount,
  formatDuration,
  formatMegabytes,
  formatPercent,
  formatQuantity,
  serverTime,
  ServerTimestampToLocalDateTimePipe,
} from '@shared/text';
import { LabelHelpComponent, StatusBadgeComponent, WaitingNoticeComponent } from '@shared/ui';
import { Title } from '@angular/platform-browser';
import { composeTitle } from '@core/title/gradient-title-strategy';
import { JobTimelineComponent } from './job-timeline.component';
import { BUILD_KIND, EXEC_KIND, jobKindName, jobSubjectLabel } from '../job-kind';
import { workerDisplayNameOrNotSharedLabel } from '../worker-label';
import { scoringRuleClassNameToReadableLabel, schedulerScoreDecimalPipeDigitsFormat } from '../scoring-rule-label';
import { abortExecJobConfirm } from '@features/exec-jobs/exec-job-status';
import {
  abortBuildConfirm,
  abortEvaluationConfirm,
  buildPhase,
  buildStatusLabel,
  evaluationAbortTarget,
  waitingAccessFrom,
  type StatusPhase,
} from '@shared/evaluation';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import {
  PendingJobSummary,
  RuleDescription,
  BuildWithOutputs,
  GradientCapabilities,
} from '@core/models';
import { links } from '@shared/routing';
import { removeAnsiEscapeSequences } from '@shared/log';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { EMPTY, Observable, tap } from 'rxjs';

const JOB_OUTCOME_BADGES: Record<string, { phase: StatusPhase; label: string }> = {
  completed: { phase: 'success', label: 'Completed' },
  failed: { phase: 'failure', label: 'Failed' },
  abandoned: { phase: 'aborted', label: 'Aborted' },
};

export function getAssignedJobName(j: AssignedJobDetail): string {
  if (j.kind === EXEC_KIND && j.exec_job) return `${j.exec_job.task}: ${jobSubjectLabel(null, j.exec_job)}`;
  if (j.kind === BUILD_KIND) return j.pname ?? j.derivations[0]?.pname ?? j.derivations[0]?.drv_path ?? j.id;
  return j.evaluation?.task ?? (j.project_display_name || j.project_name);
}

export function getPendingJobName(p: PendingJobSummary): string {
  if (p.exec_job) return `${p.exec_job.task}: ${jobSubjectLabel(null, p.exec_job)}`;
  if (p.kind === BUILD_KIND) return p.subject ?? p.build_id ?? p.evaluation_id;
  return p.task ?? p.project_name ?? p.evaluation_id;
}

interface RuleRow {
  name: string;
  label: string;
  value: number;
  share: number;
  positive: boolean;
  description: string;
}

@Component({
  selector: 'app-board-job-detail',
  standalone: true,
  imports: [CommonModule, ServerTimestampToLocalDateTimePipe, RouterModule, DialogComponent, ButtonComponent, IconComponent, PopoverComponent, JobTimelineComponent, TableComponent, LoadingSpinnerComponent, LoadStateComponent, BadgeComponent, LabelHelpComponent, StatusBadgeComponent, WaitingNoticeComponent],
  template: `
    <div class="top-row phone-icon-buttons">
      <a [routerLink]="links.board('live')" class="back" (click)="goBack($event)">
        <gr-icon name="arrow_back" size="sm" />Live Jobs
      </a>
      @if (canAbort()) {
        <button grButton label="Abort" icon="close" severity="danger" size="small"
                (click)="abort()"></button>
      }
    </div>

    @if (job(); as j) {
      <header class="head">
        <div>
          <gr-badge class="kind">{{ jobKindName(j.kind) }}</gr-badge>
          <h1>{{ getAssignedJobName(j) }}</h1>
        </div>
        <gr-status-badge [phase]="jobStatusBadge().phase" [label]="jobStatusBadge().label" />
      </header>

      <section class="ids">
        <div>
          <span class="label">Project</span>
          <a class="project-link" [routerLink]="links.project(j.project_name)">{{ j.project_display_name || j.project_name }}</a>
        </div>
        <div>
          <span class="label">Worker</span>
          @if (j.worker_name && j.can_open_worker_metrics_page) {
            <a class="mono worker-link" [routerLink]="links.workerMetrics(j.project_name, j.worker_id)">{{ j.worker_name }}</a>
          } @else {
            <span class="mono worker-name">{{ workerDisplayNameOrNotSharedLabel(j.worker_name) }}</span>
          }
        </div>
        <div>
          <span class="label">Log</span>
          @if (j.evaluation) {
            <a class="log-link" [routerLink]="links.evaluationLog(j.project_name, j.evaluation_id, { build: j.build_id })" [title]="j.evaluation_id">Open log</a>
          } @else {
            <span class="muted">Evaluation deleted</span>
          }
        </div>
        @if (j.cluster_job) {
          <div>
            <span class="label">Cluster job</span>
            <a class="mono cluster-link" [routerLink]="links.boardCluster(j.cluster_job)">{{ j.cluster_job }}</a>
          </div>
        }
      </section>

      @if (j.kind !== EXEC_KIND && j.evaluation; as ev) {
        <section class="ctx evaluation">
          <h2>Evaluation</h2>
          <gr-table class="kv">
            <tbody>
              <tr><td class="label">Repository</td><td class="mono">{{ ev.repository }}</td></tr>
              <tr>
                <td class="label">Commit</td>
                <td class="mono" [title]="ev.commit">{{ ev.commit.slice(0, 12) }}@if (ev.commit_message) { <span class="muted"> {{ ev.commit_message }}</span> }</td>
              </tr>
              <tr><td class="label">Wildcard</td><td class="mono">{{ ev.wildcard }}</td></tr>
              @if (ev.task) {
                <tr>
                  <td class="label">Task</td>
                  <td class="mono"><a class="task-link" [routerLink]="links.task(j.project_name, ev.task, j.evaluation_id)">{{ ev.task }}</a></td>
                </tr>
              }
            </tbody>
          </gr-table>
        </section>
      }

      @if (j.exec_job; as ex) {
        <section class="ctx exec">
          <h2>Exec job</h2>
          <gr-table class="kv">
            <tbody>
              <tr><td class="label">Task</td><td class="mono">{{ ex.task }}</td></tr>
              <tr>
                <td class="label">Entry point</td>
                <td class="mono"><a class="exec-link" [routerLink]="links.boardExecJob(ex)">{{ ex.entry_point }}</a></td>
              </tr>
              @if (ex.nodes > 1) {
                <tr><td class="label">Node</td><td class="mono">{{ ex.node_index }} of {{ ex.nodes }} nodes</td></tr>
              }
              @if (j.exec_attempt; as a) {
                <tr><td class="label">Attempt</td><td class="mono">{{ a.number }}</td></tr>
                @if (a.end_reason) {
                  <tr>
                    <td class="label">End reason</td>
                    <td class="mono end-reason">{{ a.end_reason }}@if (a.message) { <span class="muted"> {{ a.message }}</span> }</td>
                  </tr>
                }
              }
            </tbody>
          </gr-table>
        </section>
      }

      <section class="timeline">
        <div class="step"><span class="label">Queued</span><span>{{ j.queued_at | serverDate }}</span></div>
        <div class="step"><span class="label">Wait</span><span>{{ waitLabel() }}</span></div>
        @if (j.worker_elapsed_ms != null) {
          <div class="step"><span class="label">Ran</span><span>{{ formatDuration(j.worker_elapsed_ms) }}</span></div>
        }
      </section>

      @if (j.phases.length) {
        <h2>Worker timeline <gr-label-help doc="ui/job-board/#job-inspection" title="About the worker timeline" /></h2>
        <gr-job-timeline [phases]="j.phases" />
      }

      @if (j.derivations.length) {
        <h2>Derivations</h2>
        <div class="drv-list">
          @for (d of j.derivations; track d.derivation_build) {
            @if (d.build; as build) {
              <button type="button" class="drv-row clickable" (click)="openBuild(build)">
                <span class="mono pname">{{ d.pname ?? '-' }}</span>
                <span class="mono path">{{ d.drv_path }}</span>
              </button>
            } @else {
              <div class="drv-row">
                <span class="mono pname">{{ d.pname ?? '-' }}</span>
                <span class="mono path">{{ d.drv_path }}</span>
              </div>
            }
          }
        </div>
      }

      @if (j.previous_attempts.length > 1) {
        <section class="attempts">
          <h2>Previous Build Attempts</h2>
          <gr-table class="rules">
            <thead><tr><th>#</th><th>Mode</th><th>Outcome</th><th>Reason</th><th>When</th><th><span class="sr-only">Open</span></th></tr></thead>
            <tbody>
              @for (a of j.previous_attempts; track a.created_at; let i = $index) {
                @let current = a.dispatched_job_id === j.id;
                @let opens = !current && a.dispatched_job_id !== null;
                <tr [class.clickable]="opens" [class.current]="current"
                    [routerLink]="opens && a.dispatched_job_id ? links.boardJob(a.dispatched_job_id) : null">
                  <td>{{ i + 1 }}</td>
                  <td>{{ a.substitute ? 'substitute' : 'build' }}</td>
                  <td class="mono">{{ attemptOutcome(a.outcome) }}</td>
                  @let reason = a.failure_message ? removeAnsiEscapeSequences(a.failure_message) : '';
                  <td class="mono reason" [title]="reason">{{ reason || '-' }}</td>
                  <td>{{ a.created_at | serverDate }}</td>
                  <td>{{ opens ? '\u203a' : '' }}</td>
                </tr>
              }
            </tbody>
          </gr-table>
        </section>
      }

      @if (j.assigned_scheduler_reasoning; as s) {
        <details class="scheduler">
          <summary>Scheduler details</summary>

          <h3>Score breakdown <gr-label-help doc="reference/scheduler-policies/" title="About the scoring rules" /></h3>
          <gr-table class="rules">
            <thead><tr><th>Rule</th><th class="num">Contribution</th><th>Share</th></tr></thead>
            <tbody>
              @for (r of rules(); track r.name) {
                <tr>
                  <td>
                    {{ r.label }}
                    @if (r.description) {
                      <button type="button" class="help" [attr.aria-label]="'Explain ' + r.label" (click)="showHelp($event, r, rulePop)">?</button>
                    }
                  </td>
                  <td class="num">{{ r.value | number: scoreDigits }}</td>
                  <td class="bar-cell">
                    <div class="bar" [class.neg]="!r.positive" [style.width.%]="r.share"></div>
                  </td>
                </tr>
              } @empty {
                <tr><td colspan="3" class="muted">No per-rule breakdown recorded.</td></tr>
              }
            </tbody>
            <tfoot><tr class="total"><td>Total</td><td class="num">{{ s.score | number: scoreDigits }}</td><td></td></tr></tfoot>
          </gr-table>

          <gr-popover #rulePop>
            @if (activeRule(); as a) {
              <div class="rule-help"><strong>{{ ruleLabel(a.rule) }}</strong> <span class="mono muted">{{ a.rule }}</span><p>{{ a.description }}</p></div>
            }
          </gr-popover>

          @if (s.candidates) {
            <h3>Runner-up candidates</h3>
            <pre>{{ s.candidates | json }}</pre>
          }

          <section class="ctx worker">
            <h3>Worker context</h3>
            <gr-table class="kv">
              <tbody>
                <tr><td class="label">Architectures</td><td class="mono">{{ s.worker_context.architectures.join(', ') || '-' }}</td></tr>
                <tr><td class="label">System features</td><td class="mono">{{ s.worker_context.system_features.join(', ') || '-' }}</td></tr>
                <tr><td class="label">Capabilities</td><td class="mono">{{ capabilityList(s.worker_context.capabilities) || '-' }}</td></tr>
                <tr><td class="label">CPU count</td><td class="mono">{{ s.worker_context.cpu_count }}</td></tr>
                <tr><td class="label">CPU core score</td><td class="mono">{{ s.worker_context.cpu_core_score | number: '1.0-2' }}</td></tr>
                <tr><td class="label">CPU usage</td><td class="mono">{{ s.worker_context.cpu_usage_pct != null ? (s.worker_context.cpu_usage_pct | number: '1.0-1') + ' %' : '-' }}</td></tr>
                <tr><td class="label">RAM total</td><td class="mono">{{ formatMegabytes(s.worker_context.ram_total_mb) }}</td></tr>
                <tr><td class="label">RAM free</td><td class="mono">{{ formatMegabytes(s.worker_context.ram_free_mb) }}</td></tr>
                <tr><td class="label">RAM reserved</td><td class="mono">{{ formatMegabytes(s.worker_context.ram_reserved_mb) }}</td></tr>
                <tr><td class="label">RAM reserved, unused</td><td class="mono">{{ formatMegabytes(s.worker_context.ram_reserved_unused_mb) }}</td></tr>
                <tr><td class="label">RAM kept for held build</td><td class="mono">{{ formatMegabytes(s.worker_context.ram_reserved_for_waiting_build_mb) }}</td></tr>
                <tr><td class="label">Disk speed</td><td class="mono">{{ s.worker_context.disk_speed_mbps != null ? (s.worker_context.disk_speed_mbps | number) + ' MB/s' : '-' }}</td></tr>
                <tr><td class="label">Upload speed</td><td class="mono">{{ s.worker_context.upload_speed_mbps != null ? (s.worker_context.upload_speed_mbps | number) + ' Mbps' : '-' }}</td></tr>
                <tr><td class="label">Download speed</td><td class="mono">{{ s.worker_context.download_speed_mbps != null ? (s.worker_context.download_speed_mbps | number) + ' Mbps' : '-' }}</td></tr>
              </tbody>
            </gr-table>
          </section>

          <section class="ctx job">
            <h3>Job context</h3>
            <gr-table class="kv">
              <tbody>
                <tr><td class="label">Kind</td><td class="mono">{{ s.job_context.kind }}</td></tr>
                @if (s.job_context.kind !== 'Eval') {
                  <tr><td class="label">Architecture</td><td class="mono">{{ s.job_context.architecture }}</td></tr>
                }
                <tr><td class="label">Missing count</td><td class="mono">{{ s.job_context.missing_count ?? '-' }}</td></tr>
                <tr><td class="label">Missing NAR size</td><td class="mono">{{ formatBytes(s.job_context.missing_nar_size) }}</td></tr>
                <tr><td class="label">Outputs in store</td><td class="mono">{{ s.job_context.outputs_present ? 'yes' : 'no' }}</td></tr>
                @if (s.job_context.substitute_outputs != null) {
                  <tr><td class="label">Substituted outputs</td><td class="mono">{{ s.job_context.substitute_outputs }}</td></tr>
                }
                <tr><td class="label">Project work share</td><td class="mono">{{ s.job_context.project_work_share != null ? (s.job_context.project_work_share | number: '1.0-3') : '-' }}</td></tr>
                <tr><td class="label">Rescore count</td><td class="mono">{{ s.job_context.rescore_count }}</td></tr>
                @if (s.job_context.kind === 'Build') {
                  <tr><td class="label">Dependency count</td><td class="mono">{{ s.job_context.dependency_count ?? '-' }}</td></tr>
                  <tr><td class="label">Package name</td><td class="mono">{{ s.job_context.pname ?? '-' }}</td></tr>
                  <tr><td class="label">Closure size</td><td class="mono">{{ formatBytes(s.job_context.closure_size) }}</td></tr>
                  <tr><td class="label">Prefer local build</td><td class="mono">{{ s.job_context.prefer_local_build ? 'yes' : 'no' }}</td></tr>
                  <tr><td class="label">Fixed output</td><td class="mono">{{ s.job_context.is_fixed_output ? 'yes' : 'no' }}</td></tr>
                } @else if (s.job_context.kind === 'Eval') {
                  <tr><td class="label">Fetch flake</td><td class="mono">{{ s.job_context.fetch_flake ? 'yes' : 'no' }}</td></tr>
                }
              </tbody>
            </gr-table>

            @if (s.job_context.kind === 'Build' && s.job_context.estimate; as e) {
              <h3>Estimate</h3>
              <gr-table class="kv">
                <tbody>
                  <tr><td class="label">Cores</td><td class="mono">{{ e.cores }}</td></tr>
                  <tr><td class="label">Peak RAM</td><td class="mono">{{ formatMegabytes(e.ram_mb) }}</td></tr>
                  <tr><td class="label">Build time</td><td class="mono">{{ formatDuration(e.build_ms) }}</td></tr>
                  <tr><td class="label">Output size</td><td class="mono">{{ formatBytes(e.output_nar_bytes) }}</td></tr>
                  <tr><td class="label">Own builds</td><td class="mono">{{ e.own_samples }}</td></tr>
                  @for (b of e.similar_builds; track b[0]) {
                    <tr><td class="label">Like {{ b[0] }}</td><td class="mono">{{ b[1] | percent }} same inputs</td></tr>
                  }
                </tbody>
              </gr-table>
            }
          </section>

          @if (s.instance_context; as inst) {
            <section class="ctx instance">
              <h3>Instance context</h3>
              <gr-table class="rules">
                <thead><tr><th>Metric</th><th class="num">5m</th><th class="num">1h</th><th class="num">24h</th></tr></thead>
                <tbody>
                  @for (m of instanceWindows(inst); track m.name) {
                    <tr>
                      <td class="mono">{{ m.name }}</td>
                      @for (v of m.values; track $index) {
                        <td class="num">{{ v }}</td>
                      }
                    </tr>
                  }
                </tbody>
              </gr-table>
              <gr-table class="kv counts">
                <tbody>
                  <tr><td class="label">Active builds</td><td class="mono">{{ inst.active_builds }}</td></tr>
                  <tr><td class="label">Pending builds</td><td class="mono">{{ inst.pending_builds }}</td></tr>
                  <tr><td class="label">Total workers</td><td class="mono">{{ inst.total_workers }}</td></tr>
                  <tr><td class="label">Idle workers</td><td class="mono">{{ inst.idle_workers }}</td></tr>
                  <tr><td class="label">Fleet CPU score</td><td class="mono">{{ inst.cpu_core_score_mean != null ? (inst.cpu_core_score_mean | number: '1.0-0') : '-' }}</td></tr>
                </tbody>
              </gr-table>
            </section>
          }
        </details>
      }
    } @else if (pending(); as p) {
      <header class="head">
        <div>
          <gr-badge class="kind">{{ jobKindName(p.kind) }}</gr-badge>
          <h1>{{ getPendingJobName(p) }}</h1>
        </div>
      </header>
      @if (p.waiting_reason && p.project_name) {
        <gr-waiting-notice banner [reason]="p.waiting_reason" [project]="p.project_name" [access]="pendingAccess()" />
      } @else {
        <p class="muted">A job can wait in the queue until a matching worker can take it. Worker details appear after assignment.</p>
      }
      <section class="ids">
        @if (p.project_name; as project) {
          <div>
            <span class="label">Project</span>
            <a class="project-link" [routerLink]="links.project(project)">{{ project }}</a>
          </div>
          @if (p.task; as task) {
            <div>
              <span class="label">Task</span>
              <a [routerLink]="links.task(project, task, p.evaluation_id)">{{ task }}</a>
            </div>
          }
        }
        @if (p.exec_job; as ex) {
          <div>
            <span class="label">Exec job</span>
            <a class="mono exec-link" [routerLink]="links.boardExecJob(ex)">{{ jobSubjectLabel(p.subject, ex) }}</a>
          </div>
        } @else if (p.subject) {
          <div><span class="label">{{ p.kind === 1 ? 'Derivation' : 'Repository' }}</span><span class="mono">{{ p.subject }}</span></div>
        }
        <div>
          <span class="label">Evaluation</span>
          @if (p.project_name; as project) {
            <a [routerLink]="links.evaluationLog(project, p.evaluation_id)" [title]="p.evaluation_id">Open log</a>
          } @else {
            <span class="mono">{{ p.evaluation_id }}</span>
          }
        </div>
        @if (p.build_id) {
          <div><span class="label">Build</span><span class="mono">{{ p.build_id }}</span></div>
        }
      </section>
      <section class="timeline">
        <div class="step"><span class="label">Queued</span><span>{{ p.queued_at | serverDate }}</span></div>
        @if (p.dependency_count > 0) {
          <div class="step"><span class="label">Dependencies</span><span>{{ p.dependency_count }}</span></div>
        }
      </section>
    } @else {
      <gr-load-state [loading]="loadError() === null && !pendingJobIsNoLongerInSchedulerQueue()" [error]="loadError()" empty
                     loadingMessage="Loading job..." errorTitle="Couldn't load job" emptyTitle="This job is no longer pending" (retry)="reload()" />
    }

    <gr-dialog
      header="Build info"
      [visible]="buildDialog()"
      (visibleChange)="buildDialog.set($event)"
      width="640px"
    >
      @if (buildLoading()) {
        <gr-loading-spinner message="Loading build..." />
      } @else if (buildError()) {
        <p class="muted">{{ buildError() }}</p>
      } @else if (build(); as b) {
        <div class="build-grid">
          <div><span class="label">Build ID</span><span class="mono">{{ b.id }}</span></div>
          <div><span class="label">Status</span><span class="mono">{{ b.status }}</span></div>
          <div><span class="label">Architecture</span><span class="mono">{{ b.architecture }}</span></div>
          <div><span class="label">Worker</span><span class="mono">{{ buildWorker() }}</span></div>
          <div class="span2"><span class="label">Derivation</span><span class="mono">{{ b.derivation_path }}</span></div>
          <div><span class="label">Created</span><span>{{ b.created_at | serverDate }}</span></div>
          <div><span class="label">Updated</span><span>{{ b.updated_at | serverDate }}</span></div>
          @if (outputs(b).length) {
            <div class="span2">
              <span class="label">Outputs</span>
              @for (o of outputs(b); track o.name) {
                <div class="mono out">{{ o.name }}: {{ o.path }}</div>
              }
            </div>
          }
        </div>
      }
      <div grDialogFooter>
        <button grButton label="Close" severity="secondary" (click)="buildDialog.set(false)"></button>
      </div>
    </gr-dialog>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './job-detail.component.scss',
})
export class BoardJobDetailComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private board = inject(BoardService);
  private evaluations = inject(EvaluationsService);
  private execJobs = inject(ExecJobsService);
  private auth = inject(AuthService);
  private confirm = inject(ConfirmService);
  private destroyRef = inject(DestroyRef);
  private location = inject(Location);

  protected readonly previousNavigationWasLiveJobsList =
    inject(Router).currentNavigation()?.previousNavigation?.finalUrl?.toString().startsWith('/board/live') === true;

  job = signal<AssignedJobDetail | null>(null);
  pending = signal<PendingJobSummary | null>(null);
  loadError = signal<unknown>(null);
  pendingJobIsNoLongerInSchedulerQueue = signal(false);
  private jobId: string | null = null;
  private descriptions = signal<Map<string, string>>(new Map());
  activeRule = signal<RuleDescription | null>(null);
  build = signal<BuildWithOutputs | null>(null);
  buildDialog = signal(false);
  buildLoading = signal(false);
  buildError = signal<string | null>(null);

  goBack(event: MouseEvent): void {
    if (!this.previousNavigationWasLiveJobsList || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey) return;
    event.preventDefault();
    this.location.back();
  }

  protected superuser = computed(() => this.auth.user()?.superuser === true);

  pendingAccess = computed(() =>
    waitingAccessFrom(this.pending()?.permissions ?? [], false, this.superuser()));

  canAbort = computed(() => {
    const j = this.job();
    if (!j || j.passed_over || j.finished_at || !this.superuser()) return false;
    return j.kind !== EXEC_KIND || !!j.exec_job;
  });

  protected readonly jobKindName = jobKindName;
  protected readonly EXEC_KIND = EXEC_KIND;
  protected readonly jobSubjectLabel = jobSubjectLabel;
  protected readonly workerDisplayNameOrNotSharedLabel = workerDisplayNameOrNotSharedLabel;
  protected readonly scoreDigits = schedulerScoreDecimalPipeDigitsFormat;
  protected readonly formatBytes = formatBytes;
  protected readonly formatMegabytes = formatMegabytes;
  protected readonly formatDuration = formatDuration;
  protected readonly removeAnsiEscapeSequences = removeAnsiEscapeSequences;

  private static readonly WORKER_CAPS: (keyof GradientCapabilities)[] = [
    'federate',
    'network',
    'secrets',
    'exec',
    'eval',
    'build',
  ];

  protected readonly getAssignedJobName = getAssignedJobName;
  protected readonly getPendingJobName = getPendingJobName;
  protected readonly ruleLabel = scoringRuleClassNameToReadableLabel;

  jobStatusBadge = computed<{ phase: StatusPhase; label: string }>(() => {
    const j = this.job();
    const b = this.build();
    if (j && b && b.id === j.build_id) return { phase: buildPhase(b.status), label: buildStatusLabel(b.status) };
    if (j?.passed_over) return { phase: 'aborted', label: 'Passed over' };
    if (!j?.finished_at) return { phase: 'running', label: 'Running' };
    return JOB_OUTCOME_BADGES[j.outcome ?? ''] ?? { phase: 'success', label: 'Finished' };
  });

  rules = computed<RuleRow[]>(() => {
    const rules = this.job()?.assigned_scheduler_reasoning?.score_breakdown.rules ?? {};
    const descriptions = this.descriptions();
    const entries = Object.entries(rules);
    const maxAbs = Math.max(1e-9, ...entries.map(([, v]) => Math.abs(v)));
    return entries
      .map(([name, value]) => ({
        name,
        label: scoringRuleClassNameToReadableLabel(name),
        value,
        share: (Math.abs(value) / maxAbs) * 100,
        positive: value >= 0,
        description: descriptions.get(name) ?? '',
      }))
      .sort((a, b) => Math.abs(b.value) - Math.abs(a.value));
  });

  showHelp(event: Event, row: RuleRow, popover: PopoverComponent): void {
    this.activeRule.set({ rule: row.name, description: row.description });
    popover.toggle(event);
  }

  buildWorker = computed(() => {
    const worker = this.build()?.worker ?? null;
    const job = this.job();
    if (!worker) return '-';
    return workerDisplayNameOrNotSharedLabel(worker === job?.worker_id ? job.worker_name : null);
  });

  waitLabel = computed(() => {
    const j = this.job();
    if (!j) return '-';
    const ms = serverTime(j.dispatched_at).getTime() - serverTime(j.queued_at).getTime();
    return ms < 0 ? '-' : formatDuration(ms);
  });

  constructor() {
    runReloadIntervalWhileTabVisible(() => this.refetchJobWhileUnfinished(), 5_000);
    const title = inject(Title);
    effect(() => {
      const j = this.job();
      if (j) title.setTitle(composeTitle(getAssignedJobName(j), 'Job board'));
    });
  }

  private refetchJobWhileUnfinished(): Observable<unknown> {
    const current = this.job();
    if (!current || current.finished_at || current.passed_over) return EMPTY;
    return this.board.getJob(current.id).pipe(
      tap((d) => {
        if (this.job()?.id !== d.id) return;
        this.job.set(d);
        if (d.finished_at && d.build_id && (this.build()?.id ?? d.build_id) === d.build_id) this.loadBuild(d.build_id);
      }),
    );
  }

  ngOnInit(): void {
    this.board
      .getScoringRules()
      .subscribe((rules) => this.descriptions.set(new Map(rules.map((r) => [r.rule, r.description]))));
    // Navigating between Previous Build Attempts reuses this component, so react
    // to param changes rather than reading the snapshot once.
    this.route.paramMap.pipe(takeUntilDestroyed(this.destroyRef)).subscribe((pm) => {
      const id = pm.get('id');
      if (id) this.loadJob(id);
    });
  }

  reload(): void {
    if (this.jobId) this.loadJob(this.jobId);
  }

  private loadJob(id: string): void {
    this.jobId = id;
    this.job.set(null);
    this.pending.set(null);
    this.loadError.set(null);
    this.pendingJobIsNoLongerInSchedulerQueue.set(false);
    this.build.set(null);
    this.buildDialog.set(false);
    if (this.route.snapshot.data['pending'] === true) {
      this.loadPending(id);
      return;
    }
    this.board.getJob(id).subscribe({
      next: (d) => {
        this.job.set(d);
        if (d.build_id) {
          this.loadBuild(d.build_id);
        }
      },
      error: (error: unknown) => this.loadError.set(error),
    });
  }

  private loadPending(id: string): void {
    this.board.getPendingJobs().subscribe({
      next: (r) => {
        const match = r.jobs.find((p) => p.exec_job?.id === id || (!p.exec_job && (p.evaluation_id === id || p.build_id === id)));
        if (match) this.pending.set(match);
        else this.pendingJobIsNoLongerInSchedulerQueue.set(true);
      },
      error: (error: unknown) => this.loadError.set(error),
    });
  }

  private abortConfirm(j: AssignedJobDetail): ConfirmRequest<unknown> {
    if (j.kind === EXEC_KIND && j.exec_job) {
      const exec = j.exec_job;
      return abortExecJobConfirm(getAssignedJobName(j), () => this.execJobs.abort(exec.id));
    }
    const action = () => this.board.abortJob(j.id);
    if (j.kind === BUILD_KIND) return abortBuildConfirm(getAssignedJobName(j), action);
    return abortEvaluationConfirm(evaluationAbortTarget(getAssignedJobName(j), j.evaluation?.commit), action);
  }

  abort(): void {
    const j = this.job();
    if (!j) return;
    this.confirm.confirm(this.abortConfirm(j)).pipe(takeUntilDestroyed(this.destroyRef)).subscribe(() => this.loadJob(j.id));
  }

  openBuild(buildId: string): void {
    this.buildDialog.set(true);
    if (this.build()?.id !== buildId) {
      this.loadBuild(buildId);
    }
  }

  outputs(b: BuildWithOutputs): { name: string; path: string }[] {
    return Object.entries(b.output ?? {}).map(([name, path]) => ({ name, path }));
  }

  capabilityList(c: GradientCapabilities): string {
    return BoardJobDetailComponent.WORKER_CAPS.filter((k) => c[k]).join(', ');
  }

  attemptOutcome(o: number): string {
    return ['running', 'built', 'substituted', 'failed', 'aborted'][o] ?? String(o);
  }

  instanceWindows(inst: InstanceContextView): { name: string; values: string[] }[] {
    const rows: [string, Windowed, (v: number) => string][] = [
      ['Wait', inst.wait_secs, (v) => formatDuration(v * 1000)],
      ['Build time', inst.build_time_ms, formatDuration],
      ['Peak RAM', inst.peak_ram_mb, formatMegabytes],
      ['CPU time', inst.cpu_time_ms, formatDuration],
      ['Avg CPU', inst.avg_cpu_pct, (v) => formatQuantity(v, '%')],
      ['Disk usage', inst.disk_bytes, formatBytes],
      ['OOM rate', inst.oom_rate, formatPercent],
      ['Closure size', inst.closure_size, formatBytes],
      ['NAR size', inst.nar_size_mb, formatMegabytes],
      ['Missing paths', inst.missing_paths, formatCount],
      ['Dependency count', inst.dependency_cnt, formatCount],
      ['Completed', inst.completed, formatCount],
    ];
    return rows
      .filter(([, w]) => w.w5m !== null || w.w1h !== null || w.w24h !== null)
      .map(([name, w, format]) => ({ name, values: [w.w5m, w.w1h, w.w24h].map((v) => (v === null ? '-' : format(v))) }));
  }

  private loadBuild(buildId: string): void {
    this.buildLoading.set(true);
    this.buildError.set(null);
    this.evaluations.getBuild(buildId).subscribe({
      next: (b) => {
        this.build.set(b);
        this.buildLoading.set(false);
      },
      error: () => {
        this.buildError.set('Failed to load build.');
        this.buildLoading.set(false);
      },
    });
  }
}
