/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable, map } from 'rxjs';
import { ConfigService } from '@core/services/config.service';
import { WorkersService } from '@core/services/workers.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import {
  AccessState,
  AllowedCapabilities,
  ConnectionStatus,
  GradientCapabilities,
  TeamSummary,
  Worker,
  WorkerPatch,
  WorkerRegistration,
  changedCapabilities,
  gradientCiEntry,
  gradientCiHost,
  gradientCiKeysUrl,
  listedWorkers,
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
  LoadingSpinnerComponent,
  MenuComponent,
  MenuItem,
  MessageBannerComponent,
  MessageService,
  PageLayoutComponent,
  PopoverComponent,
  RowComponent,
  RowListComponent,
  ToastComponent,
} from '@gradient/ui/ui';
import { AllowedCapabilitiesComponent, GradientCiConnectComponent, LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, canOpenTeam } from '@shared/access';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';

const ALL_ALLOWED: AllowedCapabilities = { enable_fetch: true, enable_eval: true, enable_build: true };

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
    ToastComponent,
    LoadingSpinnerComponent,
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
  providers: [MessageService],
  templateUrl: './workers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: ['./workers.component.scss', './workers.dialog.scss'],
})
export class WorkersComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workersService = inject(WorkersService);
  private projectsService = inject(ProjectsService);
  private projectAccess = inject(ProjectAccessService);
  private messageService = inject(MessageService);
  private teamsService = inject(TeamsService);
  private authService = inject(AuthService);
  config = inject(ConfigService);
  private myTeams = signal<TeamSummary[]>([]);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  rowAccess(worker: Worker): AccessState {
    const a = this.access();
    return {
      managed: worker.managed || a.managed,
      canEdit: a.canEdit,
      canTrigger: a.canTrigger,
    };
  }

  // Activate/Deactivate and Fire Test stay available on state-managed workers:
  // state restores `active` on restart and a test changes nothing.
  actionAccess(): AccessState {
    return { ...this.access(), managed: false };
  }

  readonly capLabels: { key: keyof GradientCapabilities; label: string }[] = [
    { key: 'federate', label: 'federate' },
    { key: 'fetch',    label: 'fetch' },
    { key: 'eval',     label: 'eval' },
    { key: 'build',    label: 'build' },
  ];

  loading = signal(true);
  registering = signal(false);
  renaming = signal(false);
  deletingId = signal<string | null>(null);
  togglingId = signal<string | null>(null);
  testingId = signal<string | null>(null);
  showRegisterDialog = signal(false);
  showTokenDialog = signal(false);
  showToggleWarningDialog = signal(false);
  showRenameDialog = signal(false);
  pendingToggleWorker = signal<Worker | null>(null);
  renamingWorker = signal<Worker | null>(null);
  errorMessage = signal<string | null>(null);
  hasNoCacheSubscribed = signal(false);

  projectName = '';
  projectDisplayName = signal('');
  /** The project UUID - shown as peer_id in the register dialog. */
  projectId = signal<string>('');
  workers = signal<Worker[]>([]);
  newWorkerId = '';
  newWorkerName = '';
  newWorkerUrl = '';
  newWorkerToken = '';
  newAllowed: AllowedCapabilities = ALL_ALLOWED;
  newName = '';
  editAllowed: AllowedCapabilities = ALL_ALLOWED;
  lastRegistration = signal<WorkerRegistration | null>(null);
  tokenCopied = signal(false);
  peerIdCopied = signal(false);

  gradientCi = computed(() => gradientCiEntry(this.config.gradientCiEnabled, this.workers()));
  otherWorkers = computed(() => listedWorkers(this.workers(), this.gradientCi()));
  workerMenus = computed(() => new Map(this.otherWorkers().map((w) => [w.worker_id, this.menuFor(w)])));

  private menuFor(worker: Worker): MenuItem[] {
    const busy = this.deletingId() !== null || this.togglingId() !== null || this.testingId() !== null;
    const canEdit = this.access().canEdit;
    const rowLocked = this.rowAccess(worker).managed;
    if (worker.team) {
      return [
        ...(this.canOpenTeam(worker.team) ? [{ label: 'Manage on team', icon: 'groups', routerLink: ['/team', worker.team, 'workers'] }] : []),
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

  statusOf = (workerId: string): Observable<ConnectionStatus | undefined> =>
    this.workersService
      .getWorkers(this.projectName)
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
        this.projectDisplayName.set(project.display_name);
      },
      error: () => {},
    });
  }

  loadWorkers(): void {
    this.loading.set(true);
    this.workersService.getWorkers(this.projectName).subscribe({
      next: (workers) => {
        this.workers.set(workers);
        this.loading.set(false);
      },
      error: (err) => {
        console.error('Failed to load workers:', err);
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
    if (!this.newWorkerId.trim() || !this.newWorkerName.trim()) return;
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
        }
        this.loadWorkers();
      },
      error: (err) => {
        this.errorMessage.set(err.message || 'Failed to register worker.');
        this.registering.set(false);
      },
    });
  }

  deleteWorker(worker: Worker): void {
    this.deletingId.set(worker.worker_id);
    this.workersService.deleteWorker(this.projectName, worker.worker_id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadWorkers();
      },
      error: (err) => {
        console.error('Failed to delete worker:', err);
        this.deletingId.set(null);
      },
    });
  }

  requestToggleWorker(worker: Worker): void {
    this.pendingToggleWorker.set(worker);
    if (worker.live && worker.active) {
      // Worker is connected and being deactivated - warn user
      this.showToggleWarningDialog.set(true);
    } else {
      this.confirmToggleWorker();
    }
  }

  confirmToggleWorker(): void {
    const worker = this.pendingToggleWorker();
    if (!worker) return;
    this.showToggleWarningDialog.set(false);
    this.togglingId.set(worker.worker_id);
    this.workersService.setWorkerActive(this.projectName, worker.worker_id, !worker.active).subscribe({
      next: () => {
        this.togglingId.set(null);
        this.pendingToggleWorker.set(null);
        this.loadWorkers();
      },
      error: (err) => {
        console.error('Failed to toggle worker active state:', err);
        this.togglingId.set(null);
        this.pendingToggleWorker.set(null);
      },
    });
  }

  cancelToggleWorker(): void {
    this.showToggleWarningDialog.set(false);
    this.pendingToggleWorker.set(null);
  }

  openRenameDialog(worker: Worker): void {
    this.renamingWorker.set(worker);
    this.newName = worker.display_name;
    this.editAllowed = worker;
    this.showRenameDialog.set(true);
  }

  confirmRename(): void {
    const worker = this.renamingWorker();
    if (!worker || !this.newName.trim()) return;
    this.renaming.set(true);
    const body: WorkerPatch = changedCapabilities(worker, this.editAllowed);
    if (this.newName.trim() !== worker.display_name) body.display_name = this.newName.trim();
    this.workersService.patchWorker(this.projectName, worker.worker_id, body).subscribe({
      next: () => {
        this.renaming.set(false);
        this.showRenameDialog.set(false);
        this.renamingWorker.set(null);
        this.loadWorkers();
      },
      error: (err) => {
        console.error('Failed to update worker:', err);
        this.renaming.set(false);
      },
    });
  }

  cancelRename(): void {
    this.showRenameDialog.set(false);
    this.renamingWorker.set(null);
  }

  fireTest(worker: Worker): void {
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
      error: (err) => {
        this.testingId.set(null);
        this.messageService.add({
          severity: 'error',
          summary: 'Worker test failed',
          detail: err?.message || 'Failed to reach worker.',
        });
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

  disconnectGradientCi(worker: Worker): void {
    this.showGradientCiDisconnect.set(false);
    this.deleteWorker(worker);
  }

  closeTokenDialog(): void {
    this.showTokenDialog.set(false);
    this.lastRegistration.set(null);
  }
}
