/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { CommonModule, Location } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { IconComponent, LoadingSpinnerComponent, TableComponent } from '@gradient/ui/ui';
import { StatusBadgeComponent } from '@shared/ui';
import type { StatusPhase } from '@shared/evaluation';
import { BoardService } from '@core/services/board.service';
import type { ClusterJobDetail, ClusterMemberView, ClusterSubJobView } from '@core/models';
import { execJobLink, jobKindName } from '../job-kind';

export interface SubJobRow {
  key: string;
  member: string;
  job: ClusterSubJobView | null;
}

const STATUS_PHASES: Record<string, StatusPhase> = {
  queued: 'queued',
  running: 'running',
  completed: 'success',
  failed: 'failure',
  aborted: 'aborted',
};

export function memberLabel(member: ClusterMemberView): string {
  if (member.exec_job) return `Node ${member.exec_job.node_index}`;
  if (member.eval_index != null) return `Part ${member.eval_index}${member.primary ? ' (primary)' : ''}`;
  return member.role;
}

export function subJobRows(cluster: ClusterJobDetail): SubJobRow[] {
  return cluster.members.flatMap((member): SubJobRow[] => {
    const jobs = cluster.sub_jobs.filter((job) => job.member === member.id).reverse();
    const label = memberLabel(member);
    if (jobs.length === 0) return [{ key: member.id, member: label, job: null }];
    return jobs.map((job) => ({ key: job.id, member: label, job }));
  });
}

@Component({
  selector: 'app-board-cluster-detail',
  standalone: true,
  imports: [CommonModule, RouterModule, IconComponent, LoadingSpinnerComponent, TableComponent, StatusBadgeComponent],
  template: `
    <div class="top-row">
      <a routerLink="/board/live" class="back" (click)="goBack($event)">
        <gr-icon name="arrow_back" size="sm" />{{ cameFromApp ? 'Back' : 'Live Jobs' }}
      </a>
    </div>

    @if (cluster(); as c) {
      <header class="head">
        <div>
          <span class="kind" [class.build]="c.kind === 1">{{ jobKindName(c.kind) }}</span>
          <h1>Cluster job</h1>
        </div>
        <gr-status-badge [phase]="phase(c.status)" [label]="c.status" />
      </header>

      <section class="ids">
        <div>
          <span class="label">Project</span>
          <a class="project-link" [routerLink]="['/project', c.project_name]">{{ c.project_display_name || c.project_name }}</a>
        </div>
        @if (execJob(); as ex) {
          <div>
            <span class="label">Exec job</span>
            <a class="mono project-link" [routerLink]="execJobLink(ex)">{{ ex.task }}: {{ ex.entry_point }}</a>
          </div>
        } @else if (c.subject) {
          <div><span class="label">Subject</span><span class="mono">{{ c.subject }}</span></div>
        }
        @if (c.evaluation_id) {
          <div>
            <span class="label">Evaluation</span>
            <a class="mono project-link" [routerLink]="['/project', c.project_name, 'log', c.evaluation_id]">{{ c.evaluation_id }}</a>
          </div>
        }
      </section>

      <section class="timeline">
        <div class="step"><span class="label">Created</span><span>{{ c.created_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Updated</span><span>{{ c.updated_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Attempts</span><span class="hl">{{ c.attempts }}</span></div>
        <div class="step"><span class="label">Retry budget</span><span>{{ c.retry_budget }}</span></div>
        <div class="step"><span class="label">Same zone</span><span>{{ c.same_zone ? 'yes' : 'no' }}</span></div>
      </section>

      <h2>Sub jobs</h2>
      <gr-table class="sub-jobs">
        <thead><tr><th>Member</th><th>Worker</th><th>Attempt</th><th>Assigned</th><th>Outcome</th><th></th></tr></thead>
        <tbody>
          @for (row of rows(); track row.key) {
            @if (row.job; as job) {
              <tr class="clickable" [routerLink]="['/board/jobs', job.id]">
                <td>{{ row.member }}</td>
                <td class="mono">{{ job.worker_name ?? job.worker_id }}</td>
                <td>{{ job.attempt }}</td>
                <td>{{ job.dispatched_at | date: 'medium' }}</td>
                <td class="mono">{{ job.outcome ?? (job.finished_at ? 'finished' : 'running') }}</td>
                <td>›</td>
              </tr>
            } @else {
              <tr>
                <td>{{ row.member }}</td>
                <td colspan="5" class="muted">Waiting for a worker</td>
              </tr>
            }
          }
        </tbody>
      </gr-table>
    } @else if (notFound()) {
      <p class="muted">Cluster job not found.</p>
    } @else {
      <gr-loading-spinner message="Loading cluster job..." />
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../job-detail/job-detail.component.scss',
})
export class BoardClusterDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private board = inject(BoardService);
  private destroyRef = inject(DestroyRef);
  private location = inject(Location);

  protected readonly cameFromApp = !!inject(Router).currentNavigation()?.previousNavigation;
  protected readonly jobKindName = jobKindName;
  protected readonly execJobLink = execJobLink;

  cluster = signal<ClusterJobDetail | null>(null);
  notFound = signal(false);
  rows = computed(() => {
    const cluster = this.cluster();
    return cluster ? subJobRows(cluster) : [];
  });
  execJob = computed(() => this.cluster()?.members.find((m) => m.exec_job?.node_index === 0)?.exec_job ?? null);

  ngOnInit(): void {
    this.route.paramMap.pipe(takeUntilDestroyed(this.destroyRef)).subscribe((pm) => {
      const id = pm.get('id');
      if (id) this.load(id);
    });
  }

  phase(status: string): StatusPhase {
    return STATUS_PHASES[status] ?? 'queued';
  }

  goBack(event: MouseEvent): void {
    if (!this.cameFromApp || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey) return;
    event.preventDefault();
    this.location.back();
  }

  private load(id: string): void {
    this.cluster.set(null);
    this.notFound.set(false);
    this.board.getClusterJob(id).subscribe({
      next: (cluster) => this.cluster.set(cluster),
      error: () => this.notFound.set(true),
    });
  }
}
