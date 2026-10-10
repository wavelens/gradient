/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import {
  AllowedCapabilities,
  ConnectionStatus,
  RegisterTeamWorker,
  Team,
  TeamWorker,
  WorkerPatch,
  changedCapabilities,
  gradientCiHost,
} from '@core/models';
import { AllowedCapabilitiesComponent, GradientCiConnectComponent } from '@shared/ui';
import { Observable, map } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-team-workers',
  standalone: true,
  imports: [
    AllowedCapabilitiesComponent,
    FormsModule,
    GradientCiConnectComponent,
    RouterModule,
    BadgeComponent,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    InputDirective,
    LoadingSpinnerComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './team-workers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamWorkersComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private teams = inject(TeamsService);
  private crumbs = inject(BreadcrumbsService);
  private authService = inject(AuthService);
  config = inject(ConfigService);

  teamName = '';
  breadcrumb = computed(() => this.crumbs.team(this.teamName, { label: 'Workers' }));
  team = signal<Team | null>(null);
  workers = signal<TeamWorker[]>([]);
  loading = signal(true);
  busy = signal<string | null>(null);
  error = signal<string | null>(null);
  showRegister = signal(false);
  showGradientCiConnect = signal(false);
  issuedToken = signal<string | null>(null);
  editing = signal<TeamWorker | null>(null);
  editName = '';
  editAllowed: AllowedCapabilities = { enable_fetch: true, enable_eval: true, enable_build: true };
  form: RegisterTeamWorker = { worker_id: '', display_name: '' };

  isAdmin = computed(() => this.team()?.role === 'admin' || this.authService.user()?.superuser === true);
  canConnectGradientCi = computed(
    () => this.isAdmin() && this.config.gradientCiEnabled && !this.workers().some((w) => w.gradient_ci),
  );

  statusOf = (workerId: string): Observable<ConnectionStatus | undefined> =>
    this.teams.workers(this.teamName).pipe(map((workers) => workers.find((w) => w.worker_id === workerId)));

  get gradientCiHost(): string {
    return gradientCiHost(this.config.gradientCiUrl);
  }

  get gradientCiLabel(): string {
    return `${window.location.host} / ${this.teamName}`;
  }

  ngOnInit(): void {
    this.teamName = this.route.snapshot.paramMap.get('team') || '';
    this.teams.get(this.teamName).subscribe({
      next: (team) => {
        this.team.set(team);
        this.crumbs.rememberTeam(this.teamName, team.display_name);
      },
    });
    this.load();
  }

  load(): void {
    this.teams.workers(this.teamName).subscribe({
      next: (workers) => {
        this.workers.set(workers);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  register(): void {
    if (!this.form.worker_id || !this.form.display_name) return;
    this.busy.set('register');
    this.error.set(null);
    const worker: RegisterTeamWorker = { ...this.form, url: this.form.url?.trim() || undefined };
    this.teams.registerWorker(this.teamName, worker).subscribe({
      next: (res) => {
        this.busy.set(null);
        this.showRegister.set(false);
        this.issuedToken.set(res.token ?? null);
        this.form = { worker_id: '', display_name: '' };
        this.load();
      },
      error: (err: Error) => this.fail(err, 'Failed to register the worker.'),
    });
  }

  setActive(worker: TeamWorker, active: boolean): void {
    this.busy.set(worker.worker_id);
    this.teams.updateWorker(this.teamName, worker.worker_id, { active }).subscribe({
      next: () => {
        this.busy.set(null);
        this.load();
      },
      error: (err: Error) => this.fail(err, 'Failed to change the worker.'),
    });
  }

  openEdit(worker: TeamWorker): void {
    this.editName = worker.display_name;
    this.editAllowed = worker;
    this.editing.set(worker);
  }

  saveEdit(): void {
    const worker = this.editing();
    if (!worker || !this.editName.trim()) return;
    const patch: WorkerPatch = changedCapabilities(worker, this.editAllowed);
    if (this.editName.trim() !== worker.display_name) patch.display_name = this.editName.trim();
    this.busy.set('edit');
    this.teams.updateWorker(this.teamName, worker.worker_id, patch).subscribe({
      next: () => {
        this.busy.set(null);
        this.editing.set(null);
        this.load();
      },
      error: (err: Error) => this.fail(err, 'Failed to change the worker.'),
    });
  }

  remove(worker: TeamWorker): void {
    this.busy.set(worker.worker_id);
    this.teams.removeWorker(this.teamName, worker.worker_id).subscribe({
      next: () => {
        this.busy.set(null);
        this.load();
      },
      error: (err: Error) => this.fail(err, 'Failed to delete the worker.'),
    });
  }

  private fail(err: Error, fallback: string): void {
    this.busy.set(null);
    this.error.set(err.message || fallback);
  }
}
