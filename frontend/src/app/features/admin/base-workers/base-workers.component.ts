/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { Observable, map } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { BaseWorkerEntry, ConnectionStatus, gradientCiKeysUrl } from '@core/models';
import { AdminService } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { GradientCiConnectComponent } from '@shared/ui';

@Component({
  selector: 'app-admin-base-workers',
  standalone: true,
  imports: [
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    EmptyStateComponent,
    LoadingSpinnerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    GradientCiConnectComponent,
  ],
  templateUrl: './base-workers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class BaseWorkersComponent implements OnInit {
  private admin = inject(AdminService);
  config = inject(ConfigService);

  workers = signal<BaseWorkerEntry[]>([]);
  loading = signal(true);
  showConnect = signal(false);
  disconnecting = signal<BaseWorkerEntry | null>(null);
  canConnect = computed(() => this.config.gradientCiEnabled && !this.workers().some((w) => w.gradient_ci));
  readonly label = window.location.host;

  statusOf = (workerId: string): Observable<ConnectionStatus | undefined> =>
    this.admin.listBaseWorkers().pipe(map((rows) => rows.find((w) => w.worker_id === workerId)));

  get keysUrl(): string {
    return gradientCiKeysUrl(this.config.gradientCiUrl);
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.admin.listBaseWorkers().subscribe({
      next: (rows) => {
        this.workers.set(rows);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  disconnect(worker: BaseWorkerEntry): void {
    this.disconnecting.set(null);
    this.admin.deleteBaseWorker(worker.worker_id).subscribe({ next: () => this.load() });
  }
}
