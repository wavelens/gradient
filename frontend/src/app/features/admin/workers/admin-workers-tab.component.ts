/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { finalize } from 'rxjs';
import { BadgeComponent, ButtonComponent, LoadStateComponent, TableComponent } from '@gradient/ui/ui';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { AdminWorkerListEntry } from '@core/models';
import { confirmAndDrainWorker, formatEntityLabel, WorkerDrainingBadgeComponent } from '@shared/ui';

type AllWorkersBulkAction = 'drain' | 'undrain';

export function getWorkerOwnerDisplayName(worker: AdminWorkerListEntry): string {
  return worker.team ? formatEntityLabel(worker.team) : worker.registrations.map((r) => formatEntityLabel(r.project)).join(', ');
}

@Component({
  selector: 'app-admin-workers-tab',
  standalone: true,
  imports: [BadgeComponent, ButtonComponent, LoadStateComponent, TableComponent, WorkerDrainingBadgeComponent],
  template: `
    @if (!loadError()) {
    <div class="actions">
      <button grButton severity="danger" size="small" icon="pause" label="Drain All"
        [loading]="runningBulkAction() === 'drain'" [disabled]="drainButtonsDisabled()"
        [disabledReason]="drainAllButtonDisabledReason()" (click)="confirmAndStartAllWorkersDrain()"></button>
      <button grButton severity="secondary" size="small" icon="play_arrow" label="Undrain All"
        [loading]="runningBulkAction() === 'undrain'" [disabled]="drainButtonsDisabled()"
        [disabledReason]="undrainAllButtonDisabledReason()" (click)="cancelAllAdminWorkerDrains()"></button>
    </div>
    }
    <gr-load-state [loading]="loading()" [error]="loadError()" [empty]="workers().length === 0"
                   loadingMessage="Loading workers..." errorTitle="Failed to load workers"
                   emptyIcon="construction" emptyTitle="No workers registered"
                   emptyMessage="Register a worker on the team or project page." (retry)="load()">
      <gr-table class="workers">
        <thead><tr><th>Worker</th><th>Owner</th><th>Projects</th><th>Status</th><th class="number">Jobs</th><th></th></tr></thead>
        <tbody>
          @for (worker of workers(); track worker.worker_id) {
            <tr>
              <td>{{ worker.display_name ?? worker.worker_id }}</td>
              <td>{{ ownerName(worker) }}</td>
              <td>
                @for (project of worker.projects; track project.id; let last = $last) {
                  {{ formatEntityLabel(project) }}{{ last ? '' : ', ' }}
                } @empty {
                  <span class="muted">-</span>
                }
              </td>
              <td>
                <gr-badge [severity]="worker.connected ? 'success' : 'neutral'">{{ worker.connected ? 'Connected' : 'Offline' }}</gr-badge>
                <app-worker-drain-badge [draining]="!!worker.live?.draining" [requestedByAdmin]="worker.requested_by_admin" />
              </td>
              <td class="number">
                @if (worker.live; as live) {
                  {{ live.assigned_job_count }} / {{ live.max_concurrent_builds }}
                } @else {
                  <span class="muted">-</span>
                }
              </td>
              <td class="actions-cell">
                @if (worker.requested_by_admin) {
                  <button grButton label="Undrain" icon="play_arrow" severity="secondary" size="small"
                    [loading]="busy() === worker.worker_id" [disabled]="runningBulkAction() !== null"
                    (click)="cancelAdminRequestedWorkerDrain(worker)"></button>
                } @else if (worker.live && !worker.live.draining) {
                  <button grButton label="Drain" icon="pause" severity="secondary" size="small"
                    [disabled]="busy() === worker.worker_id || runningBulkAction() !== null" (click)="confirmAndStartWorkerDrain(worker)"></button>
                }
              </td>
            </tr>
          }
        </tbody>
      </gr-table>
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../admin-area.scss',
})
export class AdminWorkersTabComponent implements OnInit {
  private admin = inject(AdminService);
  private confirm = inject(ConfirmService);
  private notify = inject(NotifyService);

  protected readonly ownerName = getWorkerOwnerDisplayName;
  protected readonly formatEntityLabel = formatEntityLabel;

  workers = signal<AdminWorkerListEntry[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);
  busy = signal<string | null>(null);
  runningBulkAction = signal<AllWorkersBulkAction | null>(null);

  readonly drainButtonsDisabled = computed(() => this.loading() || this.busy() !== null || this.runningBulkAction() !== null);
  readonly drainAllButtonDisabledReason = computed(() => {
    if (this.loading()) return null;
    if (this.workers().length === 0) return 'The worker list is empty.';
    if (this.workers().every((w) => w.requested_by_admin)) return 'All workers already have an admin drain.';
    return null;
  });
  readonly undrainAllButtonDisabledReason = computed(() =>
    this.loading() || this.workers().some((w) => w.requested_by_admin)
      ? null
      : 'No workers have a drain that an admin set. Workers that drain themselves stop draining on restart.');

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.admin.listWorkers().subscribe({
      next: (workers) => {
        this.workers.set(workers);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private replace(updated: AdminWorkerListEntry): void {
    this.workers.update((workers) => workers.map((w) => (w.worker_id === updated.worker_id ? updated : w)));
  }

  confirmAndStartWorkerDrain(worker: AdminWorkerListEntry): void {
    confirmAndDrainWorker(this.confirm, worker.display_name ?? worker.worker_id, () => {
      this.busy.set(worker.worker_id);
      return this.admin.setWorkerDraining(worker.worker_id, true).pipe(finalize(() => this.busy.set(null)));
    }).subscribe((updated) => {
      this.replace(updated);
      this.notify.success('Drain started');
    });
  }

  confirmAndStartAllWorkersDrain(): void {
    this.confirm.confirm({
      title: 'Drain All Workers',
      target: 'All workers on this instance',
      consequence: 'Running jobs finish. These workers receive no new jobs until Undrain All. Offline workers start to drain when they connect. Server restarts end the drain.',
      confirmLabel: 'Drain All',
      failure: 'Failed to drain all workers',
      severity: 'danger',
      action: () => {
        this.runningBulkAction.set('drain');
        return this.admin.setAllWorkersDraining(true).pipe(finalize(() => this.runningBulkAction.set(null)));
      },
    }).subscribe((workers) => {
      this.workers.set(workers);
      this.notify.success('Started drain on all workers');
    });
  }

  cancelAllAdminWorkerDrains(): void {
    this.runningBulkAction.set('undrain');
    this.admin.setAllWorkersDraining(false).subscribe({
      next: (workers) => {
        this.runningBulkAction.set(null);
        this.workers.set(workers);
        this.notify.success('All admin drains stopped');
      },
      error: (error: unknown) => {
        this.runningBulkAction.set(null);
        this.notify.failure('Failed to stop all drains', error);
      },
    });
  }

  cancelAdminRequestedWorkerDrain(worker: AdminWorkerListEntry): void {
    this.busy.set(worker.worker_id);
    this.admin.setWorkerDraining(worker.worker_id, false).subscribe({
      next: (updated) => {
        this.busy.set(null);
        this.replace(updated);
        this.notify.success('Drain stopped');
      },
      error: (error: unknown) => {
        this.busy.set(null);
        this.notify.failure('Failed to stop drain', error);
      },
    });
  }
}
