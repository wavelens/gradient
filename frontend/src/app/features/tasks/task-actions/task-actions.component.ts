/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ActionsService } from '@core/services/actions.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ProjectsService } from '@core/services/projects.service';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  IconComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  BadgeSeverity,
} from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import {
  Action,
  ActionType,
  CreateActionRequest,
  Integration,
  UpdateActionRequest,
} from '@core/models';
import { ActionFormComponent } from './action-form.component';
import { ActionDeliveriesComponent } from './action-deliveries.component';

interface IntegrationOption {
  id: string;
  display_name: string;
}

@Component({
  selector: 'app-task-actions',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
    ActionFormComponent,
    ActionDeliveriesComponent,
    IconComponent,
    PageLayoutComponent,
    EmptyStateComponent,
    BadgeComponent,
  ],
  templateUrl: './task-actions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-actions.component.scss',
})
export class TaskActionsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private actionsService = inject(ActionsService);
  private integrationsService = inject(IntegrationsService);
  private projectsService = inject(ProjectsService);
  private accessSvc = inject(AccessService);

  access = injectTaskAccess();

  rowDisabled = computed(
    () =>
      this.testingId() !== null ||
      this.deletingId() !== null ||
      this.accessSvc.shouldDisableInput(this.access()),
  );

  triggerAccess = computed(() => this.accessSvc.triggerAccess(this.access()));

  triggerRowDisabled = computed(
    () =>
      this.testingId() !== null ||
      this.deletingId() !== null ||
      this.accessSvc.shouldDisableInput(this.triggerAccess()),
  );

  loading = signal(true);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  testingId = signal<string | null>(null);
  testSuccessId = signal<string | null>(null);
  testFailureId = signal<string | null>(null);

  projectName = '';
  projectDisplayName = signal('');
  taskName = '';

  actions = signal<Action[]>([]);
  outboundIntegrations = signal<IntegrationOption[]>([]);

  editingAction = signal<Action | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);

  deliveriesActionId = signal<string | null>(null);

  revealedToken = signal<string | null>(null);

  error = signal<string | null>(null);
  confirmDeleteId = signal<string | null>(null);

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.loadActions();
    this.loadIntegrations();
  }

  loadActions(): void {
    this.loading.set(true);
    this.actionsService.list(this.projectName, this.taskName).subscribe({
      next: (list) => {
        this.actions.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  private loadIntegrations(): void {
    this.integrationsService.listProjectIntegrations(this.projectName).subscribe({
      next: (list: Integration[]) =>
        this.outboundIntegrations.set(
          list
            .filter((i) => i.kind === 'outbound')
            .map((i) => ({ id: i.id, display_name: i.display_name })),
        ),
      error: () => this.outboundIntegrations.set([]),
    });
  }

  startCreate(): void {
    this.error.set(null);
    this.editingAction.set(null);
    this.showCreateDialog.set(true);
  }

  startEdit(action: Action): void {
    this.error.set(null);
    this.editingAction.set(action);
    this.showEditDialog.set(true);
  }

  onCreateSaved(request: CreateActionRequest | UpdateActionRequest): void {
    this.saving.set(true);
    this.error.set(null);
    this.actionsService.create(this.projectName, this.taskName, request as CreateActionRequest).subscribe({
      next: (res) => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        if (res.token) this.revealedToken.set(res.token);
        this.loadActions();
      },
      error: (err) => {
        this.error.set(err?.message || 'Failed to create action.');
        this.saving.set(false);
      },
    });
  }

  onEditSaved(request: CreateActionRequest | UpdateActionRequest): void {
    const target = this.editingAction();
    if (!target) return;
    this.saving.set(true);
    this.error.set(null);
    this.actionsService.update(this.projectName, this.taskName, target.id, request as UpdateActionRequest).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingAction.set(null);
        this.loadActions();
      },
      error: (err) => {
        this.error.set(err?.message || 'Failed to update action.');
        this.saving.set(false);
      },
    });
  }

  requestDelete(id: string): void {
    this.confirmDeleteId.set(id);
  }

  confirmDelete(): void {
    const id = this.confirmDeleteId();
    if (!id) return;
    this.deletingId.set(id);
    this.actionsService.delete(this.projectName, this.taskName, id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.confirmDeleteId.set(null);
        this.loadActions();
      },
      error: () => {
        this.deletingId.set(null);
        this.confirmDeleteId.set(null);
      },
    });
  }

  cancelDelete(): void {
    this.confirmDeleteId.set(null);
  }

  testAction(id: string): void {
    this.testingId.set(id);
    this.testSuccessId.set(null);
    this.testFailureId.set(null);
    this.actionsService.test(this.projectName, this.taskName, id).subscribe({
      next: () => {
        this.testingId.set(null);
        this.testSuccessId.set(id);
        setTimeout(() => {
          if (this.testSuccessId() === id) this.testSuccessId.set(null);
        }, 3000);
      },
      error: () => {
        this.testingId.set(null);
        this.testFailureId.set(id);
        setTimeout(() => {
          if (this.testFailureId() === id) this.testFailureId.set(null);
        }, 3000);
      },
    });
  }

  openDeliveries(id: string): void {
    this.deliveriesActionId.set(id);
  }

  closeDeliveries(): void {
    this.deliveriesActionId.set(null);
  }

  dismissToken(): void {
    this.revealedToken.set(null);
  }

  typeLabel(type: ActionType): string {
    switch (type) {
      case 'send_mail': return 'Send Mail';
      case 'send_web_request': return 'Web Request';
      case 'forge_status_report': return 'Forge Status';
      case 'open_pr': return 'Open PR';
    }
  }

  typeSeverity(type: ActionType): BadgeSeverity {
    switch (type) {
      case 'send_mail': return 'info';
      case 'send_web_request': return 'success';
      case 'forge_status_report': return 'warning';
      case 'open_pr': return 'neutral';
    }
  }

  typeIcon(type: ActionType): string {
    switch (type) {
      case 'send_mail': return 'mail';
      case 'send_web_request': return 'public';
      case 'forge_status_report': return 'published_with_changes';
      case 'open_pr': return 'code';
    }
  }

  relativeTime(isoString: string | null): string {
    if (!isoString) return 'never';
    const ms = Date.now() - new Date(isoString + (isoString.endsWith('Z') ? '' : 'Z')).getTime();
    const secs = Math.floor(ms / 1000);
    if (secs < 60) return `${secs}s ago`;
    const mins = Math.floor(secs / 60);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    return `${Math.floor(hours / 24)}d ago`;
  }
}
