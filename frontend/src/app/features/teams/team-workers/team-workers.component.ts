/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TeamsService } from '@core/services/teams.service';
import { inBackground } from '@core/interceptors/request-context';
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
  RegisterWorkerRequest,
  WorkerConnection,
  PatchWorkerRequest,
} from '@core/models';
import { AllowedCapabilitiesComponent, GradientCiConnectComponent } from '@shared/ui';
import { Observable, map } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
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
    BadgeComponent,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './team-workers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamWorkersComponent implements OnInit {
  private teams = inject(TeamsService);
  private authService = inject(AuthService);
  private notify = inject(NotifyService);
  config = inject(ConfigService);

  team = input.required<TeamResponse>();
  workers = signal<TeamWorkerEntry[]>([]);
  showOffline = signal(false);
  loading = signal(true);
  loadError = signal<unknown>(null);
  busy = signal<string | null>(null);
  registerError = signal<string | null>(null);
  editError = signal<string | null>(null);
  showRegister = signal(false);
  showGradientCiConnect = signal(false);
  issuedToken = signal<string | null>(null);
  editing = signal<TeamWorkerEntry | null>(null);
  editName = '';
  editAllowed: AllowedCapabilities = ALL_ALLOWED;
  form: RegisterWorkerRequest = { worker_id: '', display_name: '' };

  isAdmin = computed(() => this.team().role === 'admin' || this.authService.user()?.superuser === true);
  offlineCount = computed(() => this.workers().filter((worker) => !worker.connected).length);
  shownWorkers = computed(() => (this.showOffline() ? this.workers() : this.workers().filter((worker) => worker.connected)));
  canConnectGradientCi = computed(
    () => this.isAdmin() && this.config.gradientCiEnabled && !this.workers().some((w) => w.gradient_ci),
  );

  statusOf = (workerId: string): Observable<WorkerConnection | undefined> =>
    this.teams.workers(this.teamName, inBackground()).pipe(map((workers) => workers.find((w) => w.worker_id === workerId)));

  get teamName(): string {
    return this.team().name;
  }

  get gradientCiHost(): string {
    return gradientCiHost(this.config.gradientCiUrl);
  }

  get gradientCiLabel(): string {
    return `${window.location.host} / ${this.teamName}`;
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

  openRegister(): void {
    this.registerError.set(null);
    this.showRegister.set(true);
  }

  register(): void {
    if (!this.form.worker_id || !this.form.display_name) return;
    this.busy.set('register');
    this.registerError.set(null);
    const worker: RegisterWorkerRequest = { ...this.form, url: this.form.url?.trim() || undefined };
    this.teams.registerWorker(this.teamName, worker).subscribe({
      next: (res) => {
        this.busy.set(null);
        this.showRegister.set(false);
        this.issuedToken.set(res.token ?? null);
        this.form = { worker_id: '', display_name: '' };
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.registerError.set(apiErrorMessage(err, 'Failed to register the worker.'));
      },
    });
  }

  setActive(worker: TeamWorkerEntry, active: boolean): void {
    this.busy.set(worker.worker_id);
    this.teams.updateWorker(this.teamName, worker.worker_id, { active }).subscribe({
      next: () => {
        this.busy.set(null);
        this.notify.success(active ? 'Worker activated' : 'Worker deactivated');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.notify.failure(active ? 'Failed to activate worker' : 'Failed to deactivate worker', err);
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
    if (!worker || !this.editName.trim()) return;
    const patch: PatchWorkerRequest = changedCapabilities(worker, this.editAllowed);
    if (this.editName.trim() !== worker.display_name) patch.display_name = this.editName.trim();
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
    this.busy.set(worker.worker_id);
    this.teams.removeWorker(this.teamName, worker.worker_id).subscribe({
      next: () => {
        this.busy.set(null);
        this.notify.success('Worker deleted');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.notify.failure('Failed to delete worker', err);
      },
    });
  }
}
