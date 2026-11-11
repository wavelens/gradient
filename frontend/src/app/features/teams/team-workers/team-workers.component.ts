/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import {
  ALL_ALLOWED,
  AllowedCapabilities,
  changedCapabilities,
  gradientCiHost,
  TeamResponse,
  TeamWorkerEntry,
  WorkerConnection,
  PatchWorkerRequest,
} from '@core/models';
import { AllowedCapabilitiesComponent, confirmAndDrainWorker, GradientCiConnectComponent, WorkerDrainingBadgeComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { MANAGED_ITEM_DISABLED_REASON, canManageTeam } from '@shared/access';
import { RegisterWorkerDialogComponent } from '@shared/worker-registration-dialog/register-worker-dialog.component';
import { EMPTY, Observable, map, tap } from 'rxjs';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-team-workers',
  standalone: true,
  imports: [
    AllowedCapabilitiesComponent,
    FormsModule,
    GradientCiConnectComponent,
    RegisterWorkerDialogComponent,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
    WorkerDrainingBadgeComponent,
  ],
  templateUrl: './team-workers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamWorkersComponent implements OnInit {
  private teams = inject(TeamsService);
  private authService = inject(AuthService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected readonly managedItemDisabledReason = MANAGED_ITEM_DISABLED_REASON;
  config = inject(ConfigService);

  team = input.required<TeamResponse>();
  workers = signal<TeamWorkerEntry[]>([]);
  showOffline = signal(false);
  loading = signal(true);
  loadError = signal<unknown>(null);
  busy = signal<string | null>(null);
  editError = signal<string | null>(null);
  showRegister = signal(false);
  showGradientCiConnect = signal(false);
  editing = signal<TeamWorkerEntry | null>(null);
  editName = '';
  editAllowed: AllowedCapabilities = ALL_ALLOWED;

  isAdmin = computed(() => canManageTeam(this.team(), this.authService.user()));
  offlineCount = computed(() => this.workers().filter((worker) => !worker.connected).length);
  shownWorkers = computed(() => (this.showOffline() ? this.workers() : this.workers().filter((worker) => worker.connected)));
  canConnectGradientCi = computed(
    () => this.isAdmin() && this.config.gradientCiEnabled && !this.workers().some((w) => w.gradient_ci),
  );

  statusOf = (workerId: string): Observable<WorkerConnection | undefined> =>
    this.teams.workers(this.teamName).pipe(map((workers) => workers.find((w) => w.worker_id === workerId)));

  get teamName(): string {
    return this.team().name;
  }

  get gradientCiHost(): string {
    return gradientCiHost(this.config.gradientCiUrl);
  }

  get gradientCiLabel(): string {
    return `${window.location.host} / ${this.teamName}`;
  }

  constructor() {
    runReloadIntervalWhileTabVisible(() => this.loading() || this.loadError() !== null ? EMPTY :
      this.teams.workers(this.teamName).pipe(tap((workers) => this.workers.set(workers))));
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.teams.workers(this.teamName).subscribe({
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

  editNameError(): string | null {
    return this.editing()?.gradient_ci || this.editName.trim() ? null : 'Enter a display name.';
  }

  reactivateWorker(worker: TeamWorkerEntry): void {
    this.busy.set(worker.worker_id);
    this.teams.updateWorker(this.teamName, worker.worker_id, { active: true }).subscribe({
      next: () => {
        this.busy.set(null);
        this.notify.success('Worker activated');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.notify.failure('Failed to activate worker', err);
      },
    });
  }

  confirmDeactivateTeamWorkerAndReloadList(worker: TeamWorkerEntry): void {
    this.confirm
      .confirm({
        title: 'Disable Worker',
        target: worker.display_name,
        consequence: 'Workers stop at once and abort running jobs. Drain first to let jobs finish.',
        confirmLabel: 'Deactivate',
        failure: 'Failed to deactivate worker',
        severity: 'danger',
        action: () => this.teams.updateWorker(this.teamName, worker.worker_id, { active: false }),
      })
      .subscribe(() => {
        this.notify.success('Worker deactivated');
        this.load();
      });
  }

  confirmAndStartWorkerDrain(worker: TeamWorkerEntry): void {
    confirmAndDrainWorker(this.confirm, worker.display_name, () =>
      this.teams.updateWorker(this.teamName, worker.worker_id, { draining: true }),
    ).subscribe(() => {
      this.notify.success('Drain started');
      this.load();
    });
  }

  cancelAdminRequestedWorkerDrain(worker: TeamWorkerEntry): void {
    this.busy.set(worker.worker_id);
    this.teams.updateWorker(this.teamName, worker.worker_id, { draining: false }).subscribe({
      next: () => {
        this.busy.set(null);
        this.notify.success('Drain stopped');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.notify.failure('Failed to stop drain', err);
      },
    });
  }

  openEdit(worker: TeamWorkerEntry): void {
    this.editName = worker.display_name;
    this.editAllowed = worker;
    this.editError.set(null);
    this.editing.set(worker);
  }

  saveEdit(): void {
    const worker = this.editing();
    if (!worker || this.busy() === 'edit' || this.editNameError()) return;
    const patch: PatchWorkerRequest = changedCapabilities(worker, this.editAllowed);
    if (this.editName.trim() !== worker.display_name) patch.display_name = this.editName.trim();
    if (Object.keys(patch).length === 0) {
      this.editing.set(null);
      return;
    }
    this.busy.set('edit');
    this.editError.set(null);
    this.teams.updateWorker(this.teamName, worker.worker_id, patch).subscribe({
      next: () => {
        this.busy.set(null);
        this.editing.set(null);
        this.notify.success('Worker saved');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.editError.set(apiErrorMessage(err, 'Failed to change the worker.'));
      },
    });
  }

  remove(worker: TeamWorkerEntry): void {
    this.confirm
      .confirm({
        title: 'Delete worker',
        target: worker.display_name,
        consequence: `${worker.display_name} is deleted and stops running builds for this team.`,
        confirmLabel: 'Delete',
        failure: 'Failed to delete the worker.',
        action: () => this.teams.removeWorker(this.teamName, worker.worker_id),
      })
      .subscribe(() => {
        this.notify.success('Worker deleted');
        this.load();
      });
  }
}
