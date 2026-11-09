/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable, finalize, map, tap } from 'rxjs';
import { ConfigService } from '@core/services/config.service';
import { WorkersService } from '@core/services/workers.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import {
  AccessState,
  accessTo,
  ALL_ALLOWED,
  AllowedCapabilities,
  changedCapabilities,
  gradientCiEntry,
  gradientCiHost,
  gradientCiKeysUrl,
  listedWorkers,
  ProjectWorkerEntry,
  RegisterWorkerResponse,
  WorkerConnection,
  PatchWorkerRequest,
  CacheSubscriptionItem,
} from '@core/models';
import { ProjectNoActiveWritableCacheSubscriptionWarningComponent } from '../project-no-active-writable-cache-subscription-warning.component';
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
  MessageService,
  PageLayoutComponent,
  PopoverComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { AllowedCapabilitiesComponent, GradientCiConnectComponent, LabelHelpComponent, entityKindToIconName } from '@shared/ui';
import { MANAGED_ITEM_DISABLED_REASON, WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, requestIfProjectMember } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { fieldChecks } from '@shared/forms';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';
import { links } from '@shared/routing';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';

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
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    IconComponent, ProjectNoActiveWritableCacheSubscriptionWarningComponent,
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
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private workersService = inject(WorkersService);
  private projectsService = inject(ProjectsService);
  private resolvedProject = injectProject();
  private crumbs = inject(BreadcrumbsService);
  private messageService = inject(MessageService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  config = inject(ConfigService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageWorkers'));
  private teamGrants = viewChild(TeamGrantsComponent);

  // Activate/Deactivate and Test worker stay available on state-managed workers:
  // state restores `active` on restart and a test changes nothing.
  actionAccess(): AccessState {
    return { ...this.access(), managed: false };
  }

  loading = signal(true);
  loadError = signal<unknown>(null);
  registering = signal(false);
  registerAttempted = signal(false);
  renaming = signal(false);
  renameAttempted = signal(false);
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
  cacheSubscriptions = signal<CacheSubscriptionItem[] | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Workers' }));
  /** The project UUID - shown as peer_id in the register dialog. */
  projectId = computed(() => this.resolvedProject()?.id ?? '');
  workers = signal<ProjectWorkerEntry[]>([]);
  newWorkerId = '';
  newWorkerName = '';
  newWorkerUrl = '';
  newWorkerToken = '';
  newAllowed: AllowedCapabilities = ALL_ALLOWED;
  newName = '';
  editAllowed: AllowedCapabilities = ALL_ALLOWED;
  lastRegistration = signal<RegisterWorkerResponse | null>(null);

  gradientCi = computed(() => gradientCiEntry(this.config.gradientCiEnabled, this.workers()));
  otherWorkers = computed(() => listedWorkers(this.workers(), this.gradientCi()));
  workerMenus = computed(() => new Map(this.otherWorkers().map((w) => [w.worker_id, this.menuFor(w)])));

  workerStatusText(worker: ProjectWorkerEntry): string {
    if (!worker.active) return 'Inactive';
    const live = worker.live;
    if (live) return `Connected · ${live.assigned_job_count}/${live.max_concurrent_builds} jobs`;
    return worker.connected ? 'Connected' : 'Offline';
  }

  private menuFor(worker: ProjectWorkerEntry): MenuItem[] {
    const busy = this.deletingId() !== null || this.togglingId() !== null || this.testingId() !== null;
    const canEdit = this.access().canEdit;
    const lockedReason = this.access().managed ? MANAGED_ITEM_DISABLED_REASON : null;
    const testWorker: MenuItem = {
      label: 'Test worker',
      icon: 'bolt',
      disabled: busy,
      disabledReason: worker.connected ? undefined : 'Worker is offline',
      command: () => this.fireTest(worker),
    };
    if (worker.team) {
      const teamLink = this.teamGrants()?.teamLink(worker.team);
      return [
        ...(teamLink ? [{ label: 'Manage on team', icon: entityKindToIconName.team, routerLink: teamLink }] : []),
        ...(canEdit ? [testWorker] : []),
      ];
    }
    if (!canEdit) return [];
    return [
      { label: 'Edit', icon: 'edit', disabled: busy || this.renaming(), disabledReason: lockedReason, command: () => this.openRenameDialog(worker) },
      {
        label: worker.active ? 'Deactivate' : 'Activate',
        icon: worker.active ? 'pause' : 'play_arrow',
        disabled: busy,
        command: () => this.requestToggleWorker(worker),
      },
      testWorker,
      { separator: true },
      { label: 'Delete', icon: 'delete', danger: true, disabled: busy, disabledReason: lockedReason, command: () => this.deleteWorker(worker) },
    ];
  }
  showGradientCiConnect = signal(false);
  showGradientCiDisconnect = signal(false);
  disconnectError = signal<string | null>(null);

  statusOf = (workerId: string): Observable<WorkerConnection | undefined> =>
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
    this.loadWorkers();
    this.loadCacheSubscriptions();
  }

  constructor() {
    runReloadIntervalWhileTabVisible(() => this.fetchWorkers().pipe(tap((workers) => {
      this.workers.set(workers);
      this.loadError.set(null);
    })));
  }

  private fetchWorkers(): Observable<ProjectWorkerEntry[]> {
    return requestIfProjectMember(this.resolvedProject(), () => this.workersService.getWorkers(this.projectName));
  }

  loadWorkers(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.fetchWorkers().subscribe({
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
    requestIfProjectMember(this.resolvedProject(), () => this.projectsService.getSubscribedCaches(this.projectName)).subscribe({
      next: (caches) => this.cacheSubscriptions.set(caches),
      error: () => this.cacheSubscriptions.set(null),
    });
  }

  openRegisterDialog(): void {
    this.newWorkerId = '';
    this.newWorkerName = '';
    this.newWorkerUrl = '';
    this.newWorkerToken = '';
    this.newAllowed = ALL_ALLOWED;
    this.errorMessage.set(null);
    this.registerAttempted.set(false);
    this.showRegisterDialog.set(true);
  }

  get workerNameError(): string | null {
    return this.newWorkerName.trim() ? null : 'Enter a worker name.';
  }

  get workerIdError(): string | null {
    const id = this.newWorkerId.trim();
    return id ? fieldChecks.uuid(id) : 'Enter a worker ID.';
  }

  get workerTokenError(): string | null {
    return fieldChecks.base64(this.newWorkerToken);
  }

  get renameNameError(): string | null {
    return this.renamingWorker()?.gradient_ci || this.newName.trim() ? null : 'Enter a worker name.';
  }

  generateWorkerId(): void {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
    this.newWorkerId = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  }

  registerWorker(): void {
    if (this.registering()) return;
    this.registerAttempted.set(true);
    if (this.workerNameError || this.workerIdError || this.workerTokenError) return;
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
    this.confirm
      .confirm({
        title: 'Delete worker',
        target: worker.display_name,
        consequence: `Removal of ${worker.display_name} from this project. Builds no longer run on this worker.`,
        confirmLabel: 'Delete',
        failure: 'Failed to remove worker.',
        action: () => this.workersService.deleteWorker(this.projectName, worker.worker_id),
      })
      .subscribe(() => this.workerDeleted());
  }

  private workerDeleted(): void {
    this.notify.success('Worker deleted');
    this.loadWorkers();
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
    this.renameAttempted.set(false);
    this.showRenameDialog.set(true);
  }

  confirmRename(): void {
    const worker = this.renamingWorker();
    if (!worker || this.renaming()) return;
    this.renameAttempted.set(true);
    if (this.renameNameError) return;
    const body: PatchWorkerRequest = changedCapabilities(worker, this.editAllowed);
    if (this.newName.trim() !== worker.display_name) body.display_name = this.newName.trim();
    if (Object.keys(body).length === 0) {
      this.cancelRename();
      return;
    }
    this.renaming.set(true);
    this.renameError.set(null);
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

  openGradientCiDisconnect(): void {
    this.disconnectError.set(null);
    this.showGradientCiDisconnect.set(true);
  }

  disconnectGradientCi(worker: ProjectWorkerEntry): void {
    this.disconnectError.set(null);
    this.deletingId.set(worker.worker_id);
    this.workersService
      .deleteWorker(this.projectName, worker.worker_id)
      .pipe(finalize(() => this.deletingId.set(null)))
      .subscribe({
        next: () => {
          this.showGradientCiDisconnect.set(false);
          this.workerDeleted();
        },
        error: (error: unknown) => this.disconnectError.set(apiErrorMessage(error, 'Worker delete failed.')),
      });
  }

  closeTokenDialog(): void {
    this.showTokenDialog.set(false);
    this.lastRegistration.set(null);
  }
}
