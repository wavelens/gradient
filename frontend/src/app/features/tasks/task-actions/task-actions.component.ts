/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TeamsService } from '@core/services/teams.service';
import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { finalize } from 'rxjs';
import { ActionsService } from '@core/services/actions.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  IconComponent,
  LoadStateComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { WritableDirective, SettingsAccessStateBadgeComponent, AccessService, isViewerProjectMember, requestIfProjectMember } from '@shared/access';
import { injectTaskAccessData, injectTaskAccessTo } from '@core/resolvers/inject-access';
import {
  ACTION_TYPE_LABEL,
  ActionType,
  ActionResponse,
  CreateActionRequest,
  UpdateActionRequest,
  ActionConfig,
  GitHostConnectionSummary,
  eventNameToReadableText,
} from '@core/models';
import { ActionFormComponent } from './action-form.component';
import { OutgoingWebhookDeliveryListDialog, getDeliveryResultReason } from '@shared/outgoing-deliveries';
import { RelativeTimePipe } from '@shared/text';
import { links } from '@shared/routing';
import { TaskSettingsNavigationComponent } from '../task-settings-navigation/task-settings-navigation.component';

@Component({
  selector: 'app-task-actions',
  standalone: true,
  imports: [
    TaskSettingsNavigationComponent,
    SettingsAccessStateBadgeComponent,
    RelativeTimePipe,
    CopyFieldComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    LoadStateComponent,
    WritableDirective,
    ActionFormComponent,
    OutgoingWebhookDeliveryListDialog,
    IconComponent,
    PageLayoutComponent,
    BadgeComponent,
  ],
  templateUrl: './task-actions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-actions.component.scss',
})
export class TaskActionsComponent implements OnInit {
  protected readonly links = links;
  protected readonly getDeliveryResultReason = getDeliveryResultReason;
  private route = inject(ActivatedRoute);
  private actionsService = inject(ActionsService);
  private gitHostConnections = inject(GitHostConnectionsApi);
  private crumbs = inject(BreadcrumbsService);
  private accessSvc = inject(AccessService);
  private teamsService = inject(TeamsService);
  private tasksService = inject(TasksService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  private taskAccessData = injectTaskAccessData();
  viewerIsProjectMember = computed(() => isViewerProjectMember(this.taskAccessData()?.task));
  access = injectTaskAccessTo('manageActions');
  memberAccess = injectTaskAccessTo('manageMembers');

  rowBusy = computed(() => this.testingId() !== null);

  triggerAccess = computed(() => this.accessSvc.triggerAccess(this.access()));

  triggerRowDisabled = computed(
    () =>
      this.testingId() !== null ||
      this.accessSvc.shouldDisableInput(this.triggerAccess()),
  );

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  testingId = signal<string | null>(null);
  testSuccessId = signal<string | null>(null);
  testFailureId = signal<string | null>(null);

  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName));

  actions = signal<ActionResponse[]>([]);
  gitHostConnectionsThatCanReportCommitStatus = signal<GitHostConnectionSummary[]>([]);
  mailTeams = signal<string[]>([]);
  projectTasks = signal<{ id: string; name: string; display_name: string }[]>([]);

  editingAction = signal<ActionResponse | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);

  deliveriesActionId = signal<string | null>(null);
  openTaskActionDeliveryHistoryLoadFunctions = computed(() => {
    const actionId = this.deliveriesActionId();
    if (!actionId) return null;
    return {
      name: this.actions().find((action) => action.id === actionId)?.name ?? '',
      page: (limit: number, offset: number) => this.actionsService.listDeliveries(this.projectName, this.taskName, actionId, limit, offset),
      detail: (deliveryId: string) => this.actionsService.getDelivery(this.projectName, this.taskName, actionId, deliveryId),
    };
  });

  revealedToken = signal<string | null>(null);

  error = signal<string | null>(null);

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.loadActions();
    this.loadGitHostConnectionsThatCanReportCommitStatus();
    this.loadProjectTasks();
    this.teamsService.projectGrants(this.projectName).subscribe({
      next: (grants) => this.mailTeams.set(grants.filter((g) => g.users && !g.pending).map((g) => g.team)),
      error: () => this.mailTeams.set([]),
    });
  }

  loadActions(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.loadTaskActionsWithoutLoadingSpinner();
  }

  private loadTaskActionsWithoutLoadingSpinner(): void {
    requestIfProjectMember(this.taskAccessData()?.task, () => this.actionsService.list(this.projectName, this.taskName)).subscribe({
      next: (list) => {
        this.actions.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private loadGitHostConnectionsThatCanReportCommitStatus(): void {
    if (!this.viewerIsProjectMember()) return;
    this.gitHostConnections.list({ type: 'project', name: this.projectName }).subscribe({
      next: (list) => this.gitHostConnectionsThatCanReportCommitStatus.set(list.filter((connection) => connection.reports_status)),
      error: () => this.gitHostConnectionsThatCanReportCommitStatus.set([]),
    });
  }

  private loadProjectTasks(): void {
    this.tasksService.getTasks(this.projectName, 1, 100).subscribe({
      next: (page) => this.projectTasks.set(page.items.map((t) => ({ id: t.id, name: t.name, display_name: t.display_name }))),
      error: () => this.projectTasks.set([]),
    });
  }

  startCreate(): void {
    this.error.set(null);
    this.editingAction.set(null);
    this.showCreateDialog.set(true);
  }

  startEdit(action: ActionResponse): void {
    this.error.set(null);
    this.editingAction.set(action);
    this.showEditDialog.set(true);
  }

  onCreateSaved(request: CreateActionRequest | UpdateActionRequest): void {
    if (this.saving()) return;
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
        this.error.set(apiErrorMessage(err, 'Failed to create action.'));
        this.saving.set(false);
      },
    });
  }

  onEditSaved(request: CreateActionRequest | UpdateActionRequest): void {
    const target = this.editingAction();
    if (!target || this.saving()) return;
    this.saving.set(true);
    this.error.set(null);
    this.actionsService.update(this.projectName, this.taskName, target.id, request as UpdateActionRequest).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingAction.set(null);
        this.notify.success('Action saved');
        this.loadActions();
      },
      error: (err) => {
        this.error.set(apiErrorMessage(err, 'Failed to update action.'));
        this.saving.set(false);
      },
    });
  }

  deleteAction(action: ActionResponse): void {
    this.confirm.confirm({
      title: 'Delete Action',
      target: action.name,
      consequence: 'Events no longer trigger this action.',
      isIrreversible: true,
      confirmLabel: 'Delete',
      failure: 'Deleting the action failed.',
      action: () => this.actionsService.delete(this.projectName, this.taskName, action.id),
    }).subscribe(() => {
      this.notify.success('Action deleted');
      this.loadActions();
    });
  }

  testAction(id: string): void {
    this.testingId.set(id);
    this.testSuccessId.set(null);
    this.testFailureId.set(null);
    this.actionsService.test(this.projectName, this.taskName, id).pipe(finalize(() => this.loadTaskActionsWithoutLoadingSpinner())).subscribe({
      next: () => {
        this.testingId.set(null);
        this.testSuccessId.set(id);
        setTimeout(() => {
          if (this.testSuccessId() === id) this.testSuccessId.set(null);
        }, 3000);
      },
      error: (error: unknown) => {
        this.testingId.set(null);
        this.notify.failure("Couldn't test action", error);
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

  typeOf(action: ActionResponse): ActionType {
    return (action.config as ActionConfig).type;
  }

  typeLabel(type: ActionType): string {
    return ACTION_TYPE_LABEL[type];
  }

  typeIcon(type: ActionType): string {
    switch (type) {
      case 'send_mail': return 'mail';
      case 'send_web_request': return 'public';
      case 'open_pr': return 'code';
      case 'send_matrix_message': return 'forum';
      case 'send_slack_message': return 'chat';
      case 'start_evaluation': return 'play_arrow';
    }
  }

  eventNameToReadableText = eventNameToReadableText;
}
