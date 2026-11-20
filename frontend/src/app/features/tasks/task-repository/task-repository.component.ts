/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { LoadStateComponent, PageLayoutComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import { TaskGitHostChainStepAction, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { RepositoryAccessComponent } from './repository-access.component';
import { RepositoryGitHostConnectionAndCommitStatusComponent } from './repository-git-host-connection.component';
import { repositoryHost } from '@shared/git';
import { SettingsAccessStateBadgeComponent, requestIfProjectMember } from '@shared/access';
import { injectTaskAccessData } from '@core/resolvers/inject-access';
import { TaskGitHostChainComponent } from '@shared/git-host-connections/task-git-host-chain.component';
import { TaskSettingsNavigationComponent } from '../task-settings-navigation/task-settings-navigation.component';

@Component({
  selector: 'app-task-repository',
  standalone: true,
  imports: [
    TaskSettingsNavigationComponent,
    LoadStateComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
    RepositoryAccessComponent,
    RepositoryGitHostConnectionAndCommitStatusComponent,
    SettingsAccessStateBadgeComponent,
    TaskGitHostChainComponent,
  ],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Repository">
      <app-task-settings-navigation slot="settings-navigation" [projectUrlName]="projectName" [taskUrlName]="taskName" />
      @if (state()) {
        <app-settings-access-state-badge slot="after-title" [access]="taskAccess()?.access" kind="task" showReadOnlyNote />
      }
      @if (state(); as repo) {
        <gr-settings-section title="Status" [card]="false">
          <app-task-git-host-chain [chain]="repo.chain" [repository]="repo.repository" [gitHostType]="gitHost()"
            [deliveries]="repo.recent_deliveries" [statusPosts]="repo.status_posts" [runningAction]="chainActionRunning()"
            (action)="runChainAction($event)" />
        </gr-settings-section>
        <app-repository-access [project]="projectName" [task]="taskName" [state]="repo"
          [gitHost]="gitHost()" (changed)="load()" />
        <app-repository-git-host-connection [project]="projectName" [task]="taskName" [state]="repo"
          (saved)="state.set($event)" (changed)="load()" />
      } @else {
        <gr-load-state [loading]="loading()" [error]="loadError()" loadingMessage="Loading repository..."
          errorTitle="Failed to load repository" (retry)="load()" />
      }
    </gr-page-layout>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TaskRepositoryComponent implements OnInit, CanComponentDeactivate {
  private route = inject(ActivatedRoute);
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);
  private crumbs = inject(BreadcrumbsService);
  protected taskAccess = injectTaskAccessData();

  private access = viewChild(RepositoryAccessComponent);
  private accessElement = viewChild(RepositoryAccessComponent, { read: ElementRef });
  private gitHostConnection = viewChild(RepositoryGitHostConnectionAndCommitStatusComponent);

  projectName = '';
  taskName = '';
  state = signal<TaskRepositoryResponse | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);
  runningAction = signal<TaskGitHostChainStepAction['type'] | null>(null);

  chainActionRunning = computed<TaskGitHostChainStepAction['type'] | null>(() => {
    const connection = this.gitHostConnection();
    if (connection?.savingSelection()) return 'select_connection';
    if (connection?.savingReportStatus()) return 'turn_on_report_status';
    if (connection?.testingStatusReport()) return 'test_status_report';
    return this.runningAction();
  });
  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName));
  gitHost = computed(() => {
    const repo = this.state();
    if (!repo) return null;
    const host = repositoryHost(repo.repository);
    return repo.git_host_connection?.git_host_type ?? (host === 'gitlab.com' ? 'gitlab' : null);
  });

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') ?? '';
    this.taskName = this.route.snapshot.paramMap.get('task') ?? '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    requestIfProjectMember(this.taskAccess()?.task, () => this.tasks.getRepository(this.projectName, this.taskName)).subscribe({
      next: (state) => {
        this.state.set(state);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  runChainAction(action: TaskGitHostChainStepAction): void {
    const connection = this.gitHostConnection();
    switch (action.type) {
      case 'edit_repository_url':
        this.access()?.startEdit();
        this.accessElement()?.nativeElement.scrollIntoView({ block: 'start', behavior: 'smooth' });
        return;
      case 'select_connection':
        connection?.selectFittingConnectionOrFocusPicker();
        return;
      case 'create_connection':
        connection?.openCreateConnection({ git_host_type: action.git_host_type, base_url: action.base_url });
        return;
      case 'turn_on_report_status':
        connection?.setReportStatus(true);
        return;
      case 'test_status_report':
        connection?.testStatusReport();
        return;
      case 'install_webhook':
        this.installWebhook();
        return;
    }
  }

  private installWebhook(): void {
    if (this.runningAction()) return;
    this.runningAction.set('install_webhook');
    this.tasks.installWebhook(this.projectName, this.taskName).subscribe({
      next: () => {
        this.runningAction.set(null);
        this.notify.success('Webhook installed');
        this.load();
      },
      error: (error: unknown) => {
        this.runningAction.set(null);
        this.notify.failure("Couldn't install webhook", error);
      },
    });
  }

  canDeactivate() {
    const unsaved = this.access()?.unsaved;
    return unsaved?.changed() ? unsaved.canLeave() : true;
  }
}
