/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { CardGridComponent, LoadStateComponent, TableComponent, StatCardComponent } from '@gradient/ui/ui';
import { BoardService } from '@core/services/board.service';
import { AdminService } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { formatBytes, formatDuration, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { BoardHealth, AdminTaskDto } from '@core/models';
import { links } from '@shared/routing';

const TASK_KIND_LABELS: Record<string, string> = { deep_gc: 'Deep GC' };
const DEEP_GC_PASS_LABELS: Record<string, string> = {
  blobs: 'Blobs',
  'exec-outputs': 'Exec output',
  logs: 'Logs',
  nars: 'NARs',
  partials: 'Partial uploads',
};

export function formatMaintenanceTaskKindLabel(kind: string): string {
  return TASK_KIND_LABELS[kind] ?? kind.replace(/_/g, ' ');
}

export function formatDeepGcCheckpointLabel(checkpoint: string): string {
  const [pass, shard] = checkpoint.split('/');
  return [DEEP_GC_PASS_LABELS[pass] ?? pass, shard].filter(Boolean).join(' ');
}

@Component({
  selector: 'app-board-health',
  standalone: true,
  imports: [CommonModule, ServerTimestampToLocalDateTimePipe, CardGridComponent, StatCardComponent, RouterModule, TableComponent, LoadStateComponent],
  template: `
    <gr-load-state [loading]="healthLoading()" [error]="healthError()" [empty]="!health()"
                   loadingMessage="Loading system health..." errorTitle="Failed to load health" (retry)="refreshHealth()">
    @if (health(); as h) {
      @if (h.draining) {
        <div class="drain-banner">Instance is draining: scheduling is paused and in-flight evaluations are parked. Clears on restart.</div>
      }
      <gr-card-grid class="kpis" min="180px">
        <gr-stat-card compact label="Version" [value]="h.version" />
        <gr-stat-card compact label="Uptime" [value]="seconds(h.uptime_seconds)" />
        <gr-stat-card compact label="Sessions" [value]="h.proto_sessions" />
      </gr-card-grid>

      <h2>Process</h2>
      <div class="grid">
        <div class="cell"><span class="label">RSS</span><span>{{ bytes(h.process.resident_memory_bytes) }}</span></div>
        <div class="cell"><span class="label">Virtual</span><span>{{ bytes(h.process.virtual_memory_bytes) }}</span></div>
        <div class="cell"><span class="label">Open fds</span><span>{{ h.process.open_fds }} / {{ h.process.max_fds }}</span></div>
        <div class="cell"><span class="label">Threads</span><span>{{ h.process.threads }}</span></div>
        <div class="cell"><span class="label">CPU time</span><span>{{ seconds(h.process.cpu_seconds_total) }}</span></div>
      </div>

      <h2>Pipeline</h2>
      <div class="grid">
        <div class="cell"><span class="label">Rollup lag</span><span [class.bad]="(h.rollup_lag_seconds ?? 0) > 300">{{ h.rollup_lag_seconds != null ? seconds(h.rollup_lag_seconds) : 'no data' }}</span></div>
        <div class="cell"><span class="label">Latest bucket</span><span>{{ h.latest_rollup_bucket ? (h.latest_rollup_bucket | serverDate: 'short') : '-' }}</span></div>
        <div class="cell"><span class="label">Cache size</span><span>{{ bytes(h.cache_bytes) }}</span></div>
        <div class="cell"><span class="label">Packages</span><span>{{ h.cache_packages }}</span></div>
        <div class="cell"><span class="label">Pending deliveries</span><span>{{ h.pending_deliveries }}</span></div>
        <div class="cell"><span class="label">Failed deliveries</span><span [class.bad]="h.failed_deliveries > 0">{{ h.failed_deliveries }}</span></div>
      </div>

      <h2>Supervision</h2>
      <gr-table class="http supervision">
        <thead><tr><th>Loop</th><th>Restarts</th><th>Errors</th><th>Timeouts</th><th>Last ok</th><th>Last error</th></tr></thead>
        <tbody>
          @for (l of supervisedLoopsThatHaveRestartsErrorsOrTimeouts(); track l.name) {
            <tr>
              <td>{{ l.name }}</td>
              <td [class.bad]="l.restarts > 0">{{ l.restarts }}</td>
              <td [class.bad]="l.pass_errors > 0">{{ l.pass_errors }}</td>
              <td [class.bad]="l.pass_timeouts > 0">{{ l.pass_timeouts }}</td>
              <td>{{ l.last_ok_seconds_ago != null ? seconds(l.last_ok_seconds_ago) + ' ago' : 'never' }}</td>
              <td [class.bad]="!!l.last_error">{{ l.last_error ?? '' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="6" class="muted">
              @if (h.supervised.length) {
                All {{ h.supervised.length }} supervised loops run with no restarts, errors or timeouts.
              } @else {
                No supervised loops reported.
              }
            </td></tr>
          }
        </tbody>
      </gr-table>

      <h2>Admin</h2>
      <div class="admin-actions">
        @if (!config.githubAppEnabled) {
          <a class="btn" [routerLink]="links.admin('github-app')">Set up GitHub App</a>
        }
        <a class="btn" [routerLink]="links.admin('webhooks')">Instance Webhooks</a>
        <button class="btn" (click)="runDeepGc()">Run Deep GC</button>
        @if (h.draining) {
          <button class="btn" (click)="disableDraining()" [disabled]="drainBusy()">Disable Draining</button>
        } @else {
          <button class="btn danger" (click)="enableDraining()">Enable Draining</button>
        }
      </div>

      <gr-table class="http">
        <thead><tr><th>Task</th><th>Status</th><th>Checkpoint</th><th>Created</th><th>Finished</th><th>Error</th></tr></thead>
        <tbody>
          @for (t of tasks(); track t.id) {
            <tr>
              <td>{{ taskKindLabel(t.kind) }}</td>
              <td>{{ t.status }}</td>
              <td class="muted">{{ t.checkpoint ? checkpointLabel(t.checkpoint) : '-' }}</td>
              <td>{{ t.created_at | serverDate: 'short' }}</td>
              <td>{{ t.finished_at ? (t.finished_at | serverDate: 'short') : '-' }}</td>
              <td [class.bad]="!!t.error">{{ t.error ?? '' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="6" class="muted">No admin tasks yet.</td></tr>
          }
        </tbody>
      </gr-table>
    }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './health.component.scss',
})
export class BoardHealthComponent implements OnInit {
  protected readonly links = links;
  private board = inject(BoardService);
  private admin = inject(AdminService);
  protected config = inject(ConfigService);
  private confirm = inject(ConfirmService);
  private notify = inject(NotifyService);

  health = signal<BoardHealth | null>(null);
  healthLoading = signal(true);
  healthError = signal<unknown>(null);
  tasks = signal<AdminTaskDto[]>([]);
  drainBusy = signal(false);
  readonly supervisedLoopsThatHaveRestartsErrorsOrTimeouts = computed(() =>
    (this.health()?.supervised ?? []).filter((l) => l.restarts > 0 || l.pass_errors > 0 || l.pass_timeouts > 0 || !!l.last_error));

  readonly bytes = formatBytes;
  protected readonly taskKindLabel = formatMaintenanceTaskKindLabel;
  protected readonly checkpointLabel = formatDeepGcCheckpointLabel;

  seconds(value: number): string {
    return formatDuration(value * 1000);
  }

  private loadTasks(): void {
    this.admin.listTasks().subscribe((t) => this.tasks.set(t));
  }

  runDeepGc(): void {
    this.confirm.confirm({
      title: 'Run Deep GC',
      target: 'All storage backends',
      consequence: 'Deep GC runs delete stored files that no row in the database references. A round already in progress can start again from the beginning.',
      confirmLabel: 'Run Deep GC',
      failure: 'Failed to start deep GC.',
      severity: 'danger',
      action: () => this.admin.startDeepGc(),
    }).subscribe(() => {
      this.notify.success('Deep GC started');
      this.loadTasks();
    });
  }

  enableDraining(): void {
    this.confirm.confirm({
      title: 'Enable Draining',
      target: 'All projects on this instance',
      consequence: 'No new evaluations or builds start and in-flight evaluations wait. A disable or a server restart can end draining.',
      confirmLabel: 'Enable Draining',
      failure: 'Failed to enable draining.',
      severity: 'danger',
      action: () => this.admin.setDraining(true),
    }).subscribe(() => {
      this.notify.success('Draining enabled');
      this.refreshHealth();
    });
  }

  disableDraining(): void {
    this.drainBusy.set(true);
    this.admin.setDraining(false).subscribe({
      next: () => { this.drainBusy.set(false); this.refreshHealth(); },
      error: (error: unknown) => {
        this.drainBusy.set(false);
        this.notify.failure('Failed to disable draining', error);
      },
    });
  }

  refreshHealth(): void {
    this.healthLoading.set(true);
    this.healthError.set(null);
    this.board.getHealth().subscribe({
      next: (h) => {
        this.health.set(h);
        this.healthLoading.set(false);
      },
      error: (error: unknown) => {
        this.healthError.set(error);
        this.healthLoading.set(false);
      },
    });
  }

  ngOnInit(): void {
    this.refreshHealth();
    this.loadTasks();
  }
}
