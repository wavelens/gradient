/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { ButtonComponent, LoadStateComponent, TableComponent } from '@gradient/ui/ui';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import { formatBytes, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { AdminStorageResponse } from '@core/models';
import { confirmAndStartDeepGcForAllStorageBackends } from '../confirm-and-start-deep-gc';

export function getCacheStorageLimitLabel(maxStorageGb: number): string {
  return maxStorageGb === 0 ? 'Unlimited' : `${maxStorageGb} GB`;
}

@Component({
  selector: 'app-admin-storage-tab',
  standalone: true,
  imports: [RouterLink, ServerTimestampToLocalDateTimePipe, ButtonComponent, LoadStateComponent, TableComponent],
  template: `
    <gr-load-state [loading]="loading()" [error]="loadError()" [empty]="!storage()"
                   loadingMessage="Loading storage..." errorTitle="Failed to load storage" (retry)="load()">
    @if (storage(); as s) {
      <div class="tiles">
        <div class="tile">
          <span class="label">Free Disk Space</span>
          <span>{{ bytes(s.disk_free_bytes) }} of {{ bytes(s.disk_total_bytes) }}</span>
        </div>
      </div>

      <h2>GC</h2>
      <p class="hint">GC rounds delete evaluations that exceed the retention limit for each task and cached paths that expired.</p>
      <p class="hint">Deep GC can delete stored files that lack a reference from a database row.</p>
      <div class="actions">
        <button grButton severity="secondary" size="small" label="Run GC Now" [loading]="isStartGcRoundRequestRunning()"
          (click)="startNormalGarbageCollectionNow()"></button>
        <button grButton severity="danger" size="small" label="Run Deep GC" (click)="runDeepGc()"></button>
      </div>
      <p class="hint last-deep-gc">{{ lastFinishedDeepGarbageCollectionRemovalSentence() ?? 'Deep GC is not complete.' }}</p>

      <h2>Caches</h2>
      <gr-table class="caches">
        <thead><tr><th>Cache</th><th class="number">Packages</th><th class="number">Size</th><th class="number">Limit</th></tr></thead>
        <tbody>
          @for (cache of s.caches; track cache.id) {
            <tr>
              <td><a [routerLink]="links.cachePage(cache.name, 'settings')">{{ cache.display_name }}</a></td>
              <td class="number">{{ cache.packages }}</td>
              <td class="number">{{ bytes(cache.bytes) }}</td>
              <td class="number">{{ cacheLimit(cache.max_storage_gb) }}</td>
            </tr>
          }
        </tbody>
      </gr-table>

      <h2>Projects</h2>
      <gr-table class="projects">
        <thead><tr><th>Project</th><th class="number">Packages</th><th class="number">Size</th></tr></thead>
        <tbody>
          @for (row of s.projects; track row.project.id) {
            <tr>
              <td><a [routerLink]="links.project(row.project.name)">{{ row.project.display_name }}</a></td>
              <td class="number">{{ row.packages }}</td>
              <td class="number">{{ bytes(row.bytes) }}</td>
            </tr>
          }
        </tbody>
      </gr-table>

      <h2>Tasks</h2>
      <p class="hint">Open a task's settings page and delete old evaluations for that task.</p>
      <gr-table class="tasks">
        <thead><tr><th>Task</th><th>Project</th><th class="number">Evaluations</th><th class="number">Keep</th><th>Oldest</th><th class="number">Size</th></tr></thead>
        <tbody>
          @for (task of s.tasks; track task.id) {
            <tr>
              <td><a [routerLink]="links.taskPage(task.project.name, task.name, 'settings')">{{ task.display_name }}</a></td>
              <td>{{ task.project.display_name }}</td>
              <td class="number">{{ task.evaluations }}</td>
              <td class="number">{{ task.keep_evaluations }}</td>
              <td>{{ task.oldest_evaluation_at ? (task.oldest_evaluation_at | serverDate) : '-' }}</td>
              <td class="number">{{ bytes(task.bytes) }}</td>
            </tr>
          }
        </tbody>
      </gr-table>
    }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../admin-area.scss',
})
export class AdminStorageTabComponent implements OnInit {
  private admin = inject(AdminService);
  private confirm = inject(ConfirmService);
  private notify = inject(NotifyService);

  protected readonly links = links;
  protected readonly bytes = formatBytes;
  protected readonly cacheLimit = getCacheStorageLimitLabel;

  storage = signal<AdminStorageResponse | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);
  isStartGcRoundRequestRunning = signal(false);

  readonly lastFinishedDeepGarbageCollectionRemovalSentence = computed(() => {
    const gc = this.storage()?.last_deep_gc;
    if (!gc) return null;
    return `Last deep GC: removed ${gc.orphan_nars_removed ?? 0} of ${gc.nars_scanned ?? 0} NARs, ${gc.orphan_blobs_removed ?? 0} of ${gc.blobs_scanned ?? 0} blobs, ${gc.orphan_logs_removed ?? 0} of ${gc.logs_scanned ?? 0} logs.`;
  });

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.admin.getDiskAndStorageUsageByCacheProjectAndTask().subscribe({
      next: (storage) => {
        this.storage.set(storage);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  startNormalGarbageCollectionNow(): void {
    this.isStartGcRoundRequestRunning.set(true);
    this.admin.startGcRound().subscribe({
      next: () => {
        this.isStartGcRoundRequestRunning.set(false);
        this.notify.success('GC round started');
      },
      error: (error: unknown) => {
        this.isStartGcRoundRequestRunning.set(false);
        this.notify.failure('Failed to start GC round', error);
      },
    });
  }

  runDeepGc(): void {
    confirmAndStartDeepGcForAllStorageBackends(this.confirm, this.admin).subscribe(() => this.notify.success('Deep GC started'));
  }
}
