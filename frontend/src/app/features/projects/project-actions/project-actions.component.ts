/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { ActionsService } from '@core/services/actions.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectProjectAccess } from '@core/resolvers/inject-access';
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
  selector: 'app-project-actions',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
    ActionFormComponent,
    ActionDeliveriesComponent,
  ],
  templateUrl: './project-actions.component.html',
  styleUrl: './project-actions.component.scss',
})
export class ProjectActionsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private actionsService = inject(ActionsService);
  private integrationsService = inject(IntegrationsService);
  private orgsService = inject(OrganizationsService);
  private accessSvc = inject(AccessService);

  access = injectProjectAccess();

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

  orgName = '';
  orgDisplayName = signal('');
  projectName = '';

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
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.loadActions();
    this.loadIntegrations();
  }

  loadActions(): void {
    this.loading.set(true);
    this.actionsService.list(this.orgName, this.projectName).subscribe({
      next: (list) => {
        this.actions.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  private loadIntegrations(): void {
    this.integrationsService.listOrgIntegrations(this.orgName).subscribe({
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
    this.actionsService.create(this.orgName, this.projectName, request as CreateActionRequest).subscribe({
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
    this.actionsService.update(this.orgName, this.projectName, target.id, request as UpdateActionRequest).subscribe({
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
    this.actionsService.delete(this.orgName, this.projectName, id).subscribe({
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
    this.actionsService.test(this.orgName, this.projectName, id).subscribe({
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

  typeIcon(type: ActionType): string {
    switch (type) {
      case 'send_mail': return 'pi pi-envelope';
      case 'send_web_request': return 'pi pi-globe';
      case 'forge_status_report': return 'pi pi-github';
      case 'open_pr': return 'pi pi-code';
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
