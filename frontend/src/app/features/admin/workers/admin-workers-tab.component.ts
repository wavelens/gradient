/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { BadgeComponent, ButtonComponent, LoadStateComponent, TableComponent } from '@gradient/ui/ui';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { AdminWorkerListEntry } from '@core/models';
import { confirmAndDrainWorker, formatEntityLabel, WorkerDrainingBadgeComponent } from '@shared/ui';

export function getWorkerOwnerDisplayName(worker: AdminWorkerListEntry): string {
  return worker.team ? formatEntityLabel(worker.team) : worker.registrations.map((r) => formatEntityLabel(r.project)).join(', ');
}

@Component({
  selector: 'app-admin-workers-tab',
  standalone: true,
  imports: [BadgeComponent, ButtonComponent, LoadStateComponent, TableComponent, WorkerDrainingBadgeComponent],
  template: `
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
                    [loading]="busy() === worker.worker_id" (click)="cancelAdminRequestedWorkerDrain(worker)"></button>
                } @else if (worker.live && !worker.live.draining) {
                  <button grButton label="Drain" icon="pause" severity="secondary" size="small"
                    [disabled]="busy() === worker.worker_id" (click)="confirmAndStartWorkerDrain(worker)"></button>
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
    confirmAndDrainWorker(this.confirm, worker.display_name ?? worker.worker_id, () => this.admin.setWorkerDraining(worker.worker_id, true)).subscribe((updated) => {
      this.replace(updated);
      this.notify.success('Drain started');
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
