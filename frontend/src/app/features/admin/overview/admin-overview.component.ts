/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { EMPTY, Observable, forkJoin, tap } from 'rxjs';
import { ButtonComponent, LoadStateComponent, TableComponent } from '@gradient/ui/ui';
import { AdminService } from '@core/services/admin.service';
import { BoardService } from '@core/services/board.service';
import { NotifyService } from '@core/services/notify.service';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { formatBytes, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { AdminStorageResponse, AdminTaskDto, AdminWorkerListEntry, BoardHealth, PendingJobsResponse } from '@core/models';
import { confirmAndStartDeepGcForAllStorageBackends } from '../confirm-and-start-deep-gc';

const adminTaskListPollIntervalMs = 5_000;
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
  const [pass] = checkpoint.split('/');
  return DEEP_GC_PASS_LABELS[pass] ?? pass;
}

@Component({
  selector: 'app-admin-overview',
  standalone: true,
  imports: [RouterLink, ServerTimestampToLocalDateTimePipe, ButtonComponent, LoadStateComponent, TableComponent],
  template: `
    <gr-load-state [loading]="loading()" [error]="loadError()" [empty]="!health()"
                   loadingMessage="Loading admin overview..." errorTitle="Failed to load admin overview" (retry)="load()">
    @if (health(); as h) {
      <div class="tiles">
        <a class="tile" [routerLink]="links.jobBoardPendingViewUrl()">
          <span class="label">Pending Jobs</span><span>{{ pendingJobsTotalCount() }}</span>
        </a>
        <a class="tile" [routerLink]="links.admin('workers')">
          <span class="label">Connected workers</span><span>{{ connectedWorkerCount() }} of {{ workers().length }}</span>
        </a>
        <a class="tile" [routerLink]="links.admin('workers')">
          <span class="label">Draining workers</span><span>{{ drainingWorkerCount() }}</span>
        </a>
        <a class="tile" [routerLink]="links.admin('storage')">
          <span class="label">Free Disk Space</span>
          <span>{{ bytes(storage()?.disk_free_bytes) }}</span>
        </a>
        <div class="tile">
          <span class="label">Running Maintenance</span><span>{{ adminMaintenanceTaskPendingOrRunningCount() }}</span>
        </div>
        <a class="tile" [routerLink]="links.settings('invites')">
          <span class="label">Pending Requests</span><span>{{ pending.count() }}</span>
        </a>
      </div>

      <h2>Maintenance</h2>
      @if (h.draining) {
        <div class="drain-banner">Instance is draining: scheduling is paused and in-flight evaluations are parked. Clears on restart.</div>
      }
      <div class="actions">
        <button grButton severity="secondary" size="small" label="Run Deep GC" (click)="runDeepGc()"></button>
        @if (h.draining) {
          <button grButton severity="secondary" size="small" label="Disable Draining" [loading]="drainBusy()" (click)="disableDraining()"></button>
        } @else {
          <button grButton severity="danger" size="small" label="Enable Draining" (click)="enableDraining()"></button>
        }
      </div>

      <gr-load-state [loading]="tasks() === null && taskListLoadError() === null" [empty]="tasks() === null" [error]="taskListLoadError()"
                     errorTitle="Failed to load admin tasks" flat (retry)="loadTasks()">
        <gr-table>
          <thead><tr><th>Task</th><th>Status</th><th>Step</th><th>Created</th><th>Finished</th><th>Error</th></tr></thead>
          <tbody>
            @for (t of tasks() ?? []; track t.id) {
              <tr>
                <td>{{ taskKindLabel(t.kind) }}</td>
                <td>{{ t.status }}</td>
                <td class="muted">{{ t.checkpoint && t.status !== 'completed' ? checkpointLabel(t.checkpoint) : '-' }}</td>
                <td>{{ t.created_at | serverDate }}</td>
                <td>{{ t.finished_at ? (t.finished_at | serverDate) : '-' }}</td>
                <td [class.bad]="!!t.error">{{ t.error ?? '' }}</td>
              </tr>
            } @empty {
              <tr><td colspan="6" class="muted">No admin tasks yet.</td></tr>
            }
          </tbody>
        </gr-table>
      </gr-load-state>
    }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../admin-area.scss',
})
export class AdminStatusTilesWithMaintenanceActionsComponent implements OnInit {
  private admin = inject(AdminService);
  private board = inject(BoardService);
  private confirm = inject(ConfirmService);
  private notify = inject(NotifyService);
  protected readonly pending = inject(PendingUserDecisionsService);

  protected readonly links = links;
  protected readonly bytes = formatBytes;
  protected readonly taskKindLabel = formatMaintenanceTaskKindLabel;
  protected readonly checkpointLabel = formatDeepGcCheckpointLabel;

  loading = signal(true);
  loadError = signal<unknown>(null);
  health = signal<BoardHealth | null>(null);
  workers = signal<AdminWorkerListEntry[]>([]);
  storage = signal<AdminStorageResponse | null>(null);
  pendingJobs = signal<PendingJobsResponse | null>(null);
  tasks = signal<AdminTaskDto[] | null>(null);
  taskListLoadError = signal<unknown>(null);
  drainBusy = signal(false);

  readonly connectedWorkerCount = computed(() => this.workers().filter((w) => w.connected).length);
  readonly drainingWorkerCount = computed(() => this.workers().filter((w) => w.live?.draining).length);
  readonly pendingJobsTotalCount = computed(() => {
    const pending = this.pendingJobs();
    return pending ? pending.jobs.length + pending.other_pending : 0;
  });
  readonly adminMaintenanceTaskPendingOrRunningCount = computed(
    () => (this.tasks() ?? []).filter((t) => t.status === 'pending' || t.status === 'running').length,
  );

  constructor() {
    runReloadIntervalWhileTabVisible(
      () => (this.adminMaintenanceTaskPendingOrRunningCount() > 0 ? this.requestAdminMaintenanceTasks() : EMPTY),
      adminTaskListPollIntervalMs,
    );
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    forkJoin({
      health: this.board.getHealth(),
      workers: this.admin.listWorkers(),
      storage: this.admin.getDiskAndStorageUsageByCacheProjectAndTask(),
      pendingJobs: this.board.getPendingJobs(),
      requests: this.pending.fetchAndStorePendingDecisions(),
    }).subscribe({
      next: (overview) => {
        this.health.set(overview.health);
        this.workers.set(overview.workers);
        this.storage.set(overview.storage);
        this.pendingJobs.set(overview.pendingJobs);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
    this.loadTasks();
  }

  private requestAdminMaintenanceTasks(): Observable<AdminTaskDto[]> {
    return this.admin.listTasks().pipe(tap((t) => this.tasks.set(t)));
  }

  loadTasks(): void {
    this.taskListLoadError.set(null);
    this.requestAdminMaintenanceTasks().subscribe({ error: (error: unknown) => this.taskListLoadError.set(error) });
  }

  private reloadInstanceHealthAndAdminMaintenanceTasks(): void {
    this.board.getHealth().subscribe((health) => this.health.set(health));
    this.loadTasks();
  }

  runDeepGc(): void {
    confirmAndStartDeepGcForAllStorageBackends(this.confirm, this.admin).subscribe(() => {
      this.notify.success('Deep GC started');
      this.reloadInstanceHealthAndAdminMaintenanceTasks();
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
      this.reloadInstanceHealthAndAdminMaintenanceTasks();
    });
  }

  disableDraining(): void {
    this.drainBusy.set(true);
    this.admin.setDraining(false).subscribe({
      next: () => { this.drainBusy.set(false); this.reloadInstanceHealthAndAdminMaintenanceTasks(); },
      error: (error: unknown) => {
        this.drainBusy.set(false);
        this.notify.failure('Failed to disable draining', error);
      },
    });
  }
}
