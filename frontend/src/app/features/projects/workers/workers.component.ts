/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable, finalize, map, tap } from 'rxjs';
import { ConfigService } from '@core/services/config.service';
import { WorkersService } from '@core/services/workers.service';
import { inBackground } from '@core/interceptors/request-context';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import {
  AccessState,
  ALL_ALLOWED,
  AllowedCapabilities,
  changedCapabilities,
  gradientCiEntry,
  gradientCiHost,
  gradientCiKeysUrl,
  listedWorkers,
  TeamSummary,
  ProjectWorkerEntry,
  GradientCapabilities,
  RegisterWorkerResponse,
  WorkerConnection,
  PatchWorkerRequest,
} from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  EmptyStateComponent,
  FieldRowComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadStateComponent,
  MenuComponent,
  MenuItem,
  MessageBannerComponent,
  MessageService,
  PageLayoutComponent,
  PopoverComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { AllowedCapabilitiesComponent, GradientCiConnectComponent, LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, canOpenTeam } from '@shared/access';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';

@Component({
  selector: 'app-workers',
  standalone: true,
  imports: [
    AllowedCapabilitiesComponent,
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    IconComponent, MessageBannerComponent,
    PageLayoutComponent,
    FormFieldComponent,
    FieldRowComponent,
    EmptyStateComponent,
    BadgeComponent,
    RowListComponent,
    RowComponent,
    MenuComponent,
    PopoverComponent,
    CopyFieldComponent,
    GradientCiConnectComponent,
    TeamGrantsComponent,
  ],
  templateUrl: './workers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: ['./workers.component.scss', './workers.dialog.scss'],
})
export class WorkersComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workersService = inject(WorkersService);
  private projectsService = inject(ProjectsService);
  private projectAccess = inject(ProjectAccessService);
  private crumbs = inject(BreadcrumbsService);
  private messageService = inject(MessageService);
  private notify = inject(NotifyService);
  private teamsService = inject(TeamsService);
  private authService = inject(AuthService);
  config = inject(ConfigService);
  private myTeams = signal<TeamSummary[]>([]);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  // Activate/Deactivate and Fire Test stay available on state-managed workers:
  // state restores `active` on restart and a test changes nothing.
  actionAccess(): AccessState {
    return { ...this.access(), managed: false };
  }

  readonly capLabels: { key: keyof GradientCapabilities; label: string }[] = [
    { key: 'federate', label: 'federate' },
    { key: 'network',  label: 'network' },
    { key: 'secrets',  label: 'secrets' },
    { key: 'exec',     label: 'exec' },
    { key: 'eval',     label: 'eval' },
    { key: 'build',    label: 'build' },
  ];

  loading = signal(true);
  loadError = signal<unknown>(null);
  registering = signal(false);
  renaming = signal(false);
  deletingId = signal<string | null>(null);
  togglingId = signal<string | null>(null);
  testingId = signal<string | null>(null);
  showRegisterDialog = signal(false);
  showTokenDialog = signal(false);
  showToggleWarningDialog = signal(false);
  showRenameDialog = signal(false);
  pendingToggleWorker = signal<ProjectWorkerEntry | null>(null);
  renamingWorker = signal<ProjectWorkerEntry | null>(null);
  errorMessage = signal<string | null>(null);
  renameError = signal<string | null>(null);
  toggleError = signal<string | null>(null);
  hasNoCacheSubscribed = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Workers' }));
  /** The project UUID - shown as peer_id in the register dialog. */
  projectId = signal<string>('');
  workers = signal<ProjectWorkerEntry[]>([]);
  newWorkerId = '';
  newWorkerName = '';
  newWorkerUrl = '';
  newWorkerToken = '';
  newAllowed: AllowedCapabilities = ALL_ALLOWED;
  newName = '';
  editAllowed: AllowedCapabilities = ALL_ALLOWED;
  lastRegistration = signal<RegisterWorkerResponse | null>(null);
  tokenCopied = signal(false);
  peerIdCopied = signal(false);

  gradientCi = computed(() => gradientCiEntry(this.config.gradientCiEnabled, this.workers()));
  otherWorkers = computed(() => listedWorkers(this.workers(), this.gradientCi()));
  workerMenus = computed(() => new Map(this.otherWorkers().map((w) => [w.worker_id, this.menuFor(w)])));

  private menuFor(worker: ProjectWorkerEntry): MenuItem[] {
    const busy = this.deletingId() !== null || this.togglingId() !== null || this.testingId() !== null;
    const canEdit = this.access().canEdit;
    const rowLocked = this.access().managed;
    if (worker.team) {
      return [
        ...(this.canOpenTeam(worker.team) ? [{ label: 'Manage on team', icon: 'groups', routerLink: ['/team', worker.team] }] : []),
        ...(canEdit ? [{ label: 'Fire Test', icon: 'bolt', disabled: busy, command: () => this.fireTest(worker) }] : []),
      ];
    }
    if (!canEdit) return [];
    return [
      { label: 'Edit', icon: 'edit', disabled: busy || this.renaming() || rowLocked, command: () => this.openRenameDialog(worker) },
      {
        label: worker.active ? 'Deactivate' : 'Activate',
        icon: worker.active ? 'pause' : 'play_arrow',
        disabled: busy,
        command: () => this.requestToggleWorker(worker),
      },
      { label: 'Fire Test', icon: 'bolt', disabled: busy, command: () => this.fireTest(worker) },
      { separator: true },
      { label: 'Delete', icon: 'delete', danger: true, disabled: busy || rowLocked, command: () => this.deleteWorker(worker) },
    ];
  }
  showGradientCiConnect = signal(false);
  showGradientCiDisconnect = signal(false);
  disconnectError = signal<string | null>(null);

  statusOf = (workerId: string): Observable<WorkerConnection | undefined> =>
    this.workersService
      .getWorkers(this.projectName, inBackground())
      .pipe(map((workers) => workers.find((w) => w.worker_id === workerId)));

  get gradientCiLabel(): string {
    return `${window.location.host} / ${this.projectName}`;
  }

  get gradientCiHost(): string {
    return gradientCiHost(this.config.gradientCiUrl);
  }

  get gradientCiKeysUrl(): string {
    return gradientCiKeysUrl(this.config.gradientCiUrl);
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((s) => this.access.set(s));
    this.loadProjectId();
    this.loadWorkers();
    this.loadCacheSubscriptions();
    this.teamsService.list().subscribe({ next: (mine) => this.myTeams.set(mine), error: () => {} });
  }

  canOpenTeam(team: string): boolean {
    return canOpenTeam(this.authService.user(), this.myTeams(), team);
  }

  private loadProjectId(): void {
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => {
        this.projectId.set(project.id);
        this.crumbs.rememberProject(this.projectName, project.display_name);
      },
      error: () => {},
    });
  }

  loadWorkers(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.workersService.getWorkers(this.projectName).subscribe({
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

  private loadCacheSubscriptions(): void {
    this.projectsService.getSubscribedCaches(this.projectName).subscribe({
      next: (caches) => this.hasNoCacheSubscribed.set(caches.length === 0),
      error: () => this.hasNoCacheSubscribed.set(false),
    });
  }

  openRegisterDialog(): void {
    this.newWorkerId = '';
    this.newWorkerName = '';
    this.newWorkerUrl = '';
    this.newWorkerToken = '';
    this.newAllowed = ALL_ALLOWED;
    this.errorMessage.set(null);
    this.showRegisterDialog.set(true);
  }

  registerWorker(): void {
    if (this.registering() || !this.newWorkerId.trim() || !this.newWorkerName.trim()) return;
    this.registering.set(true);
    this.errorMessage.set(null);
    const url = this.newWorkerUrl.trim() || undefined;
    const token = this.newWorkerToken.trim() || undefined;
    this.workersService.registerWorker(this.projectName, this.newWorkerId.trim(), this.newWorkerName.trim(), url, token, this.newAllowed).subscribe({
      next: (reg) => {
        this.registering.set(false);
        this.showRegisterDialog.set(false);
        // Only show the token dialog when there is something to display.
        if (reg.token || this.newWorkerToken.trim()) {
          this.lastRegistration.set(reg);
          this.tokenCopied.set(false);
          this.showTokenDialog.set(true);
        } else {
          this.notify.success('Worker registered');
        }
        this.loadWorkers();
      },
      error: (error: unknown) => {
        this.errorMessage.set(apiErrorMessage(error, 'Failed to register worker.'));
        this.registering.set(false);
      },
    });
  }

  deleteWorker(worker: ProjectWorkerEntry): void {
    this.removeWorker(worker).subscribe({
      error: (error: unknown) => this.notify.failure('Worker delete failed', error),
    });
  }

  private removeWorker(worker: ProjectWorkerEntry): Observable<unknown> {
    this.deletingId.set(worker.worker_id);
    return this.workersService.deleteWorker(this.projectName, worker.worker_id).pipe(
      tap(() => {
        this.notify.success('Worker deleted');
        this.loadWorkers();
      }),
      finalize(() => this.deletingId.set(null)),
    );
  }

  requestToggleWorker(worker: ProjectWorkerEntry): void {
    if (worker.live && worker.active) {
      this.pendingToggleWorker.set(worker);
      this.toggleError.set(null);
      this.showToggleWarningDialog.set(true);
      return;
    }
    this.setActive(worker).subscribe({
      error: (error: unknown) =>
        this.notify.failure(worker.active ? 'Worker deactivation failed' : 'Worker activation failed', error),
    });
  }

  confirmToggleWorker(): void {
    const worker = this.pendingToggleWorker();
    if (!worker || this.togglingId() !== null) return;
    this.toggleError.set(null);
    this.setActive(worker).subscribe({
      next: () => {
        this.showToggleWarningDialog.set(false);
        this.pendingToggleWorker.set(null);
      },
      error: (error: unknown) => this.toggleError.set(apiErrorMessage(error, 'Worker deactivation failed.')),
    });
  }

  private setActive(worker: ProjectWorkerEntry): Observable<unknown> {
    this.togglingId.set(worker.worker_id);
    return this.workersService.setWorkerActive(this.projectName, worker.worker_id, !worker.active).pipe(
      tap(() => {
        this.notify.success(worker.active ? 'Worker deactivated' : 'Worker activated');
        this.loadWorkers();
      }),
      finalize(() => this.togglingId.set(null)),
    );
  }

  cancelToggleWorker(): void {
    this.showToggleWarningDialog.set(false);
    this.pendingToggleWorker.set(null);
  }

  openRenameDialog(worker: ProjectWorkerEntry): void {
    this.renamingWorker.set(worker);
    this.newName = worker.display_name;
    this.editAllowed = worker;
    this.renameError.set(null);
    this.showRenameDialog.set(true);
  }

  confirmRename(): void {
    const worker = this.renamingWorker();
    if (!worker || this.renaming() || !this.newName.trim()) return;
    this.renaming.set(true);
    this.renameError.set(null);
    const body: PatchWorkerRequest = changedCapabilities(worker, this.editAllowed);
    if (this.newName.trim() !== worker.display_name) body.display_name = this.newName.trim();
    this.workersService.patchWorker(this.projectName, worker.worker_id, body).subscribe({
      next: () => {
        this.renaming.set(false);
        this.showRenameDialog.set(false);
        this.renamingWorker.set(null);
        this.notify.success('Worker updated');
        this.loadWorkers();
      },
      error: (error: unknown) => {
        this.renameError.set(apiErrorMessage(error, 'Worker update failed.'));
        this.renaming.set(false);
      },
    });
  }

  cancelRename(): void {
    this.showRenameDialog.set(false);
    this.renamingWorker.set(null);
  }

  fireTest(worker: ProjectWorkerEntry): void {
    this.testingId.set(worker.worker_id);
    this.workersService.testWorker(this.projectName, worker.worker_id).subscribe({
      next: (r) => {
        this.testingId.set(null);
        this.messageService.add({
          severity: r.ok ? 'success' : 'error',
          summary: r.ok ? 'Worker reachable' : 'Worker test failed',
          detail: r.message,
        });
      },
      error: (error: unknown) => {
        this.testingId.set(null);
        this.notify.failure('Worker test failed', error);
      },
    });
  }

  copyToken(): void {
    const token = this.lastRegistration()?.token;
    if (token) {
      navigator.clipboard.writeText(token).then(() => {
        this.tokenCopied.set(true);
        setTimeout(() => this.tokenCopied.set(false), 2000);
      });
    }
  }

  copyPeerId(): void {
    const peerId = this.projectId();
    if (peerId) {
      navigator.clipboard.writeText(peerId).then(() => {
        this.peerIdCopied.set(true);
        setTimeout(() => this.peerIdCopied.set(false), 2000);
      });
    }
  }

  openGradientCiDisconnect(): void {
    this.disconnectError.set(null);
    this.showGradientCiDisconnect.set(true);
  }

  disconnectGradientCi(worker: ProjectWorkerEntry): void {
    this.disconnectError.set(null);
    this.removeWorker(worker).subscribe({
      next: () => this.showGradientCiDisconnect.set(false),
      error: (error: unknown) => this.disconnectError.set(apiErrorMessage(error, 'Worker delete failed.')),
    });
  }

  closeTokenDialog(): void {
    this.showTokenDialog.set(false);
    this.lastRegistration.set(null);
  }
}
