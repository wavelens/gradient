/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { RepositoryAccessComponent } from './repository-access.component';
import { RepositoryGitHostConnectionAndCommitStatusComponent } from './repository-git-host-connection.component';
import { repositoryHost } from '@shared/git';
import { SettingsAccessStateBadgeComponent, requestIfProjectMember } from '@shared/access';
import { injectTaskAccessData } from '@core/resolvers/inject-access';
import { TaskSettingsNavigationComponent } from '../task-settings-navigation/task-settings-navigation.component';

@Component({
  selector: 'app-task-repository',
  standalone: true,
  imports: [TaskSettingsNavigationComponent, LoadStateComponent, PageLayoutComponent, RepositoryAccessComponent, RepositoryGitHostConnectionAndCommitStatusComponent, SettingsAccessStateBadgeComponent],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Repository">
      <app-task-settings-navigation slot="settings-navigation" [projectUrlName]="projectName" [taskUrlName]="taskName" />
      @if (state()) {
        <app-settings-access-state-badge slot="after-title" [access]="taskAccess()?.access" kind="task" showReadOnlyNote />
      }
      @if (state(); as repo) {
        <app-repository-access [project]="projectName" [task]="taskName" [state]="repo"
          [gitHost]="gitHost()" (changed)="load()" />
        <app-repository-git-host-connection [project]="projectName" [task]="taskName" [state]="repo"
          (taskRepositoryResponseAfterGitHostSave)="state.set($event)" (changed)="load()" />
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
  private crumbs = inject(BreadcrumbsService);
  protected taskAccess = injectTaskAccessData();

  private access = viewChild(RepositoryAccessComponent);
  private gitHostConnection = viewChild(RepositoryGitHostConnectionAndCommitStatusComponent);

  projectName = '';
  taskName = '';
  state = signal<TaskRepositoryResponse | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);

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

  canDeactivate() {
    const unsaved = [this.access()?.unsaved, this.gitHostConnection()?.unsaved].find((form) => form?.changed());
    return unsaved ? unsaved.canLeave() : true;
  }
}
